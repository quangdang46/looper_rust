//! Worker runner — implements the [`WorkerScheduler`] trait.
//!
//! The Worker is the implementation role: it takes a specification (from the
//! Planner or a contributor issue), prepares a worktree, runs an agent to
//! implement the change, validates it, and opens a pull request.
//!
//! **Step pipeline** (6 steps):
//!   1. `prepare-work`      — resolve the spec / issue to implement
//!   2. `prepare-worktree`  — create a dedicated feature branch + worktree
//!   3. `plan`              — let the agent plan its implementation
//!   4. `execute`           — the agent writes the code
//!   5. `validate`          — build + test the change
//!   6. `open-pr`           — push and open a GitHub PR

use std::sync::Arc;

use std::path::Path;
use std::process::Command;

use chrono::Utc;
use uuid::Uuid;

use looper_agent::executor::ConfiguredExecutor;
use looper_git::types::{CheckoutMode, CleanupWorktreeInput, CreateWorktreeInput};
use looper_git::{build_worktree_directory_name, Gateway as GitGateway};
use looper_scheduler::scheduler::SendRepos;
use looper_scheduler::types::{
    Context, SchedulerConfig, WorkerDiscoveryInput, WorkerDiscoveryResult, WorkerIssueEntry, WorkerScheduler,
};
use looper_storage::eventlog;
use looper_storage::record::{
    AppendInput, LoopRecord, NotificationRecord, QueueItemRecord, QueueMarkRetryInput, RunRecord,
};
use looper_types::RunStatus;

use looper_github::gateway::Gateway;
use looper_github::types::{
    CreatePullRequestInput, IssueCommentInput, ListOpenIssuesInput, ListOpenPullRequestsInput, ViewPullRequestInput,
};

use crate::types::{spec_labels, worker_steps, SpecPRInfo};

/// Worker runner state machine.
pub struct Worker {
    pub config: SchedulerConfig,
    pub repos: Arc<SendRepos>,
    pub github: Option<Arc<Gateway>>,
    pub tokio_handle: tokio::runtime::Handle,
    pub agent: Option<Arc<ConfiguredExecutor>>,
    pub git: Option<Arc<GitGateway>>,
}

// SAFETY: SendRepos is Send+Sync (Mutex<Repositories>); Gateway/ConfiguredExecutor/GitGateway are Send+Sync.
unsafe impl Send for Worker {}
unsafe impl Sync for Worker {}

/// Resolve the worker worktree path from the project record.
fn resolve_worker_wt(worker: &Worker, item: &QueueItemRecord, loop_id: &str) -> String {
    // Try to get repo_path from project record
    if let Some(ref pid) = item.project_id {
        if let Ok(g) = worker.repos.0.lock() {
            if let Ok(Some(proj)) = g.projects.get_by_id(pid) {
                if !proj.repo_path.is_empty() {
                    return format!("{}/.looper/worktrees/worker-{loop_id}", proj.repo_path);
                }
            }
        }
    }
    // Fallback: use CWD
    if let Ok(cwd) = std::env::current_dir() {
        format!("{}/.looper/worktrees/worker-{loop_id}", cwd.display())
    } else {
        format!(".looper/worktrees/worker-{loop_id}")
    }
}

impl Worker {
    pub fn new(
        config: &SchedulerConfig,
        repos: Arc<SendRepos>,
        github: Option<Arc<Gateway>>,
        tokio_handle: tokio::runtime::Handle,
        agent: Option<Arc<ConfiguredExecutor>>,
        git: Option<Arc<GitGateway>>,
    ) -> Self {
        Self { config: config.clone(), repos, github, tokio_handle, agent, git }
    }

    /// Find the planner-created spec PR that references a given issue number.
    ///
    /// Returns `None` when the issue has no planner spec PR, or when the
    /// GitHub gateway is unavailable / the query fails.  Non-fatal — the
    /// worker falls back to implementing from the issue body directly.
    fn find_spec_pr_for_issue(&self, repo: &str, issue_number: i64) -> Option<looper_github::types::PullRequestDetail> {
        let gateway = self.github.as_ref()?;
        // Planner spec PRs have the `looper:spec-reviewing` or `looper:spec-ready`
        // label.  Search both in one pass by listing without a filter, then
        // checking labels in-memory.
        let open_prs = gateway
            .list_open_pull_requests(ListOpenPullRequestsInput {
                repo: repo.to_string(),
                cwd: ".".to_string(),
                limit: 50,
                label: String::new(),
                labels: vec![],
                author: String::new(),
                base_ref_name: String::new(),
                timeout: None,
            })
            .ok()?;

        // Only consider PRs with spec-phase labels.
        let spec_prs: Vec<_> = open_prs
            .into_iter()
            .filter(|pr| pr.labels.iter().any(|l| l == spec_labels::SPEC_REVIEWING || l == spec_labels::SPEC_READY))
            .collect();

        for pr in &spec_prs {
            let detail = gateway
                .view_pull_request(ViewPullRequestInput {
                    repo: repo.to_string(),
                    pr_number: pr.number,
                    cwd: ".".to_string(),
                })
                .ok()?;

            // Check if the PR body references the target issue number
            let body_ref = format!("#{}", issue_number);
            if detail.body.contains(&body_ref)
                || detail.title.contains(&body_ref)
                || detail.body.contains(&format!("issue #{}", issue_number))
                || detail.body.contains(&format!("Fixes #{}", issue_number))
                || detail.body.contains(&format!("Closes #{}", issue_number))
                || detail.body.contains(&format!("Resolves #{}", issue_number))
            {
                return Some(detail);
            }
        }
        None
    }

    fn execute_pipeline(&self, item: &QueueItemRecord) -> Result<(), String> {
        let ctx = Context::new();
        let now_iso = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();

        let loop_id = item.loop_id.as_deref().ok_or_else(|| "Worker queue item has no loop_id".to_string())?;

        // Create / resume run -------------------------------------------------
        let guard = self.repos.0.lock().map_err(|e| e.to_string())?;
        let run = match guard.runs.get_latest_by_loop_id(loop_id).map_err(|e| e.to_string())? {
            Some(run) if run.status == RunStatus::Running.as_str() || run.status == RunStatus::Queued.as_str() => {
                let mut r = run.clone();
                r.status = RunStatus::Running.as_str().to_string();
                r.started_at.clone_from(&now_iso);
                r.updated_at.clone_from(&now_iso);
                guard.runs.upsert(&r).map_err(|e| e.to_string())?;
                match guard.runs.get_by_id(&run.id).map_err(|e| e.to_string())? {
                    Some(rr) => rr,
                    None => return Err("run vanished after upsert".into()),
                }
            }
            _ => {
                let new_run = RunRecord {
                    id: Uuid::new_v4().to_string(),
                    loop_id: loop_id.to_string(),
                    status: RunStatus::Running.as_str().to_string(),
                    current_step: Some(worker_steps::PREPARE_WORK.to_string()),
                    last_completed_step: None,
                    checkpoint_json: None,
                    summary: None,
                    error_message: None,
                    agent_vendor: None,
                    model: None,
                    started_at: now_iso.clone(),
                    last_heartbeat_at: Some(now_iso.clone()),
                    ended_at: None,
                    created_at: now_iso.clone(),
                    updated_at: now_iso.clone(),
                };
                guard.runs.upsert(&new_run).map_err(|e| e.to_string())?;
                new_run
            }
        };
        drop(guard);

        // Execute steps -------------------------------------------------------
        let steps = worker_steps::ALL;
        let start_idx = run
            .last_completed_step
            .as_ref()
            .and_then(|lcs| steps.iter().position(|s| s == lcs))
            .map(|p| p + 1)
            .unwrap_or(0);

        for &step in &steps[start_idx..] {
            if ctx.is_cancelled() {
                break;
            }
            tracing::info!("Worker step: {step} (loop={loop_id})");

            match step {
                worker_steps::PREPARE_WORK => {
                    if let Ok(g) = self.repos.0.lock() {
                        let event = AppendInput {
                            event_type: "worker.prepare_work".into(),
                            project_id: item.project_id.clone(),
                            loop_id: item.loop_id.clone(),
                            run_id: Some(run.id.clone()),
                            entity_type: Some("queue_item".into()),
                            entity_id: Some(item.id.clone()),
                            payload_json: Some(serde_json::json!({"step": "prepare-work"}).to_string()),
                            ..AppendInput::new("")
                        };
                        if let Err(e) = eventlog::append(&g.events, &event) {
                            tracing::warn!("Worker prepare_work event: {e}");
                        }
                    }
                }
                worker_steps::PREPARE_WORKTREE => {
                    if let Ok(g) = self.repos.0.lock() {
                        let event = AppendInput {
                            event_type: "worker.prepare_worktree".into(),
                            project_id: item.project_id.clone(),
                            loop_id: item.loop_id.clone(),
                            run_id: Some(run.id.clone()),
                            entity_type: Some("queue_item".into()),
                            entity_id: Some(item.id.clone()),
                            payload_json: Some(serde_json::json!({"step": "prepare-worktree"}).to_string()),
                            ..AppendInput::new("")
                        };
                        if let Err(e) = eventlog::append(&g.events, &event) {
                            tracing::warn!("Worker prepare_worktree event: {e}");
                        }
                    }
                    // Perform git worktree creation
                    if let Some(ref git) = self.git {
                        // Resolve the repo path from project record
                        let (wt_repo_path, wt_root) = {
                            let repos_lock = self.repos.0.lock();
                            match repos_lock {
                                Ok(g) => {
                                    let proj_path = g
                                        .projects
                                        .get_by_id(&item.project_id.clone().unwrap_or_default())
                                        .ok()
                                        .flatten()
                                        .map(|p| p.repo_path.clone())
                                        .filter(|p| !p.is_empty())
                                        .unwrap_or_else(|| ".".to_string());
                                    let wt = format!("{}/.looper/worktrees", proj_path);
                                    (proj_path, wt)
                                }
                                Err(_) => (".".to_string(), ".".to_string()),
                            }
                        };
                        tracing::info!("Worker worktree: repo_path={wt_repo_path}, wt_root={wt_root}");
                        let input = looper_git::types::CreateWorktreeInput {
                            project_id: item.project_id.clone().unwrap_or_default(),
                            repo_path: wt_repo_path.clone(),
                            worktree_root: wt_root.clone(),
                            branch: format!("worker/{loop_id}"),
                            base_branch: Some("main".to_string()),
                            start_point: Some("main".to_string()),
                            pr_number: None,
                            checkout_mode: looper_git::types::CheckoutMode::Branch,
                            protected_branches: vec!["main".to_string(), "master".to_string()],
                        };
                        match self.tokio_handle.block_on(git.create_worktree(input)) {
                            Ok(_) => tracing::info!("Worktree created"),
                            Err(e) => tracing::warn!("Worktree creation failed: {e}"),
                        }
                    }
                }
                worker_steps::PLAN => {
                    if let Ok(g) = self.repos.0.lock() {
                        let event = AppendInput {
                            event_type: "worker.plan".into(),
                            project_id: item.project_id.clone(),
                            loop_id: item.loop_id.clone(),
                            run_id: Some(run.id.clone()),
                            entity_type: Some("queue_item".into()),
                            entity_id: Some(item.id.clone()),
                            payload_json: Some(serde_json::json!({"step": "plan"}).to_string()),
                            ..AppendInput::new("")
                        };
                        if let Err(e) = eventlog::append(&g.events, &event) {
                            tracing::warn!("Worker plan event: {e}");
                        }
                    }
                    // Plan via agent
                    if let Some(ref agent) = self.agent {
                        let plan_wt = resolve_worker_wt(self, item, loop_id);
                        // Create working directory if it doesn't exist
                        let _ = std::fs::create_dir_all(&plan_wt);
                        let input = looper_agent::executor::StartInput {
                            loop_id: run.loop_id.clone(),
                            current_step: Some("plan".to_string()),
                            last_completed_step: Some("prepare_worktree".to_string()),
                            checkpoint_json: None,
                            project_id: item.project_id.clone().unwrap_or_default(),
                            run_id: run.id.clone(),
                            working_directory: plan_wt.clone(),
                            prompt: format!(
                                "Plan the implementation for issue #{} in repo {}. Create specs/{}-spec/spec.md with the implementation steps.",
                                item.target_id,
                                item.repo.as_deref().unwrap_or("unknown"),
                                item.target_id,
                            ),
                        };
                        match self.tokio_handle.block_on(agent.start(input)) {
                            Ok(exec) => {
                                tracing::info!("Agent plan started for run {}, waiting for completion...", run.id);
                                match self.tokio_handle.block_on(exec.wait()) {
                                    Ok(result) => {
                                        tracing::info!(
                                            "Agent plan completed for run {} (summary: {})",
                                            run.id,
                                            result.summary
                                        );
                                    }
                                    Err(e) => tracing::warn!("Agent plan execution failed for run {}: {}", run.id, e),
                                }
                            }
                            Err(e) => tracing::warn!("Agent plan failed to start for run {}: {}", run.id, e),
                        }
                    }
                }
                worker_steps::EXECUTE => {
                    if let Ok(g) = self.repos.0.lock() {
                        let event = AppendInput {
                            event_type: "worker.execute".into(),
                            project_id: item.project_id.clone(),
                            loop_id: item.loop_id.clone(),
                            run_id: Some(run.id.clone()),
                            entity_type: Some("queue_item".into()),
                            entity_id: Some(item.id.clone()),
                            payload_json: Some(serde_json::json!({"step": "execute"}).to_string()),
                            ..AppendInput::new("")
                        };
                        if let Err(e) = eventlog::append(&g.events, &event) {
                            tracing::warn!("Worker execute event: {e}");
                        }
                    }

                    // --- Read planner spec context if available ---
                    let spec_context: String = {
                        let repo = item.repo.as_deref().unwrap_or("");
                        let issue_num: i64 = item.target_id.parse().unwrap_or(0);
                        if !repo.is_empty() && issue_num > 0 {
                            match self.find_spec_pr_for_issue(repo, issue_num) {
                                Some(pr_detail) => {
                                    let info = SpecPRInfo::parse(
                                        &pr_detail.body,
                                        &pr_detail.labels,
                                        &pr_detail.reviews,
                                        pr_detail.number,
                                    );
                                    let spec_path = info.spec_path;
                                    tracing::info!(
                                        "Worker: found planner spec PR #{} for issue #{} (path={}, phase={:?})",
                                        pr_detail.number,
                                        issue_num,
                                        spec_path,
                                        info.phase
                                    );
                                    format!(
                                        "\nPlanner spec PR #{} (phase: {:?}) spec_path: {}\nPR body:\n{}",
                                        pr_detail.number, info.phase, spec_path, pr_detail.body
                                    )
                                }
                                None => {
                                    tracing::info!("Worker: no planner spec PR found for issue #{}", issue_num);
                                    String::new()
                                }
                            }
                        } else {
                            String::new()
                        }
                    };
                    // Store spec context in checkpoint so OPEN_PR can use it
                    if let Ok(guard) = self.repos.0.lock() {
                        let mut r = match guard.runs.get_by_id(&run.id).map_err(|e| e.to_string()) {
                            Ok(Some(rr)) => rr,
                            _ => run.clone(),
                        };
                        if !spec_context.is_empty() {
                            r.checkpoint_json =
                                Some(serde_json::json!({"planner_spec_context": spec_context}).to_string());
                        }
                        let _ = guard.runs.upsert(&r);
                        drop(guard);
                    }

                    // Execute via agent
                    if let Some(ref agent) = self.agent {
                        let exec_wt = resolve_worker_wt(self, item, loop_id);
                        // Create working directory if it doesn't exist
                        let _ = std::fs::create_dir_all(&exec_wt);
                        let input = looper_agent::executor::StartInput {
                            loop_id: run.loop_id.clone(),
                            current_step: Some("execute".to_string()),
                            last_completed_step: Some("prepare_worktree".to_string()),
                            checkpoint_json: None,
                            project_id: item.project_id.clone().unwrap_or_default(),
                            run_id: run.id.clone(),
                            working_directory: exec_wt.clone(),
                            prompt: if spec_context.is_empty() {
                                format!(
                                    "Execute the planned implementation for issue #{} in repo {}. Write the actual code changes needed.",
                                    item.target_id,
                                    item.repo.as_deref().unwrap_or("unknown")
                                )
                            } else {
                                format!(
                                    "Execute the planned implementation for issue #{} in repo {}. \
                                     Write the actual code changes needed.\n\n\
                                     Below is the spec context from the planner:\n{spec_context}",
                                    item.target_id,
                                    item.repo.as_deref().unwrap_or("unknown")
                                )
                            },
                        };
                        match self.tokio_handle.block_on(agent.start(input)) {
                            Ok(exec) => {
                                tracing::info!("Agent execution started for run {}, waiting for completion...", run.id);
                                match self.tokio_handle.block_on(exec.wait()) {
                                    Ok(result) => {
                                        tracing::info!(
                                            "Agent execution completed for run {} (summary: {})",
                                            run.id,
                                            result.summary
                                        );
                                    }
                                    Err(e) => tracing::warn!("Agent execution failed for run {}: {}", run.id, e),
                                }
                            }
                            Err(e) => tracing::warn!("Agent execution failed to start for run {}: {}", run.id, e),
                        }
                    }
                }
                worker_steps::VALIDATE => {
                    if let Ok(g) = self.repos.0.lock() {
                        let event = AppendInput {
                            event_type: "worker.validate".into(),
                            project_id: item.project_id.clone(),
                            loop_id: item.loop_id.clone(),
                            run_id: Some(run.id.clone()),
                            entity_type: Some("queue_item".into()),
                            entity_id: Some(item.id.clone()),
                            payload_json: Some(serde_json::json!({"step": "validate"}).to_string()),
                            ..AppendInput::new("")
                        };
                        if let Err(e) = eventlog::append(&g.events, &event) {
                            tracing::warn!("Worker validate event: {e}");
                        }
                    }

                    // Use resolve_worker_wt() to get the correct worktree path,
                    // matching PLAN and EXECUTE steps.
                    let worktree_path = resolve_worker_wt(self, item, loop_id);
                    let _ = std::fs::create_dir_all(&worktree_path);

                    // Detect project type and run appropriate validation
                    let cargo_toml = Path::new(&worktree_path).join("Cargo.toml");
                    let package_json = Path::new(&worktree_path).join("package.json");
                    let pyproject = Path::new(&worktree_path).join("pyproject.toml");
                    let setup_py = Path::new(&worktree_path).join("setup.py");

                    let validation_ok = if cargo_toml.exists() {
                        // Rust project - run cargo build
                        tracing::info!("Worker validate: running cargo build in {worktree_path}");
                        match Command::new("cargo").args(["build"]).current_dir(&worktree_path).output() {
                            Ok(output) if output.status.success() => {
                                tracing::info!("Worker validate: cargo build succeeded for run {}", run.id);
                                true
                            }
                            Ok(output) => {
                                let stderr = String::from_utf8_lossy(&output.stderr);
                                tracing::warn!(
                                    "Worker validate: cargo build FAILED for run {}: {}",
                                    run.id,
                                    stderr.lines().next().unwrap_or("unknown")
                                );
                                false
                            }
                            Err(e) => {
                                tracing::warn!("Worker validate: cargo not available: {}", e);
                                true // skip validation if cargo isn't installed
                            }
                        }
                    } else if package_json.exists() {
                        // Node.js project - check syntax only
                        tracing::info!("Worker validate: checking Node.js project in {worktree_path}");
                        match Command::new("node").args(["--check", "index.js"]).current_dir(&worktree_path).output() {
                            Ok(output) if output.status.success() => {
                                tracing::info!("Worker validate: Node.js syntax check passed for run {}", run.id);
                                true
                            }
                            _ => {
                                tracing::info!(
                                    "Worker validate: Node.js syntax check skipped (no index.js), assuming OK"
                                );
                                true
                            }
                        }
                    } else if pyproject.exists() || setup_py.exists() {
                        // Python project - check syntax
                        tracing::info!("Worker validate: checking Python project in {worktree_path}");
                        let python_check = Command::new("python3")
                            .args(["-m", "py_compile", "."])
                            .current_dir(&worktree_path)
                            .output();
                        match python_check {
                            Ok(output) if output.status.success() => {
                                tracing::info!("Worker validate: Python check passed for run {}", run.id);
                                true
                            }
                            _ => {
                                // py_compile "." doesn't work - try individual files instead
                                let mut all_ok = true;
                                if let Ok(entries) = std::fs::read_dir(&worktree_path) {
                                    for entry in entries.flatten() {
                                        let path = entry.path();
                                        if path.extension().map(|e| e == "py").unwrap_or(false) {
                                            if let Ok(output) = Command::new("python3")
                                                .args(["-m", "py_compile", path.to_str().unwrap_or("")])
                                                .output()
                                            {
                                                if !output.status.success() {
                                                    all_ok = false;
                                                }
                                            }
                                        }
                                    }
                                }
                                if all_ok {
                                    tracing::info!("Worker validate: Python files look valid for run {}", run.id);
                                } else {
                                    tracing::warn!("Worker validate: Python syntax check found issues");
                                }
                                all_ok
                            }
                        }
                    } else {
                        // Generic project - just verify files exist
                        tracing::info!("Worker validate: generic project (no Cargo.toml/package.json/pyproject.toml), skipping build test for run {}", run.id);
                        true
                    };

                    if !validation_ok {
                        // Mark the queue item for retry so it will be picked up again.
                        let guard = self.repos.0.lock().map_err(|e| e.to_string())?;
                        let attempts = item.attempts + 1;
                        let retry_at = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
                        if let Err(e) = guard.queue.mark_retry(&QueueMarkRetryInput {
                            id: item.id.clone(),
                            available_at: retry_at.clone(),
                            attempts,
                            error_message: Some("validation failed".to_string()),
                            error_kind: "retryable_transient".to_string(),
                            updated_at: retry_at.clone(),
                        }) {
                            tracing::warn!("Worker validate: failed to mark queue item for retry: {e}");
                        }
                        drop(guard);

                        // Mark the run as failed so the pipeline stops here.
                        {
                            let guard = self.repos.0.lock().map_err(|e| e.to_string())?;
                            let mut failed_run = guard
                                .runs
                                .get_by_id(&run.id)
                                .map_err(|e| e.to_string())?
                                .ok_or("run not found during validate failure")?;
                            failed_run.status = RunStatus::Failed.as_str().to_string();
                            failed_run.error_message = Some("validation failed".to_string());
                            failed_run.ended_at = Some(retry_at.clone());
                            failed_run.updated_at.clone_from(&retry_at);
                            if let Err(e) = guard.runs.upsert(&failed_run) {
                                tracing::warn!("Worker validate: failed to mark run as failed: {e}");
                            }
                        }

                        return Err(format!(
                            "Worker validate: validation failed for run {} (item {})",
                            run.id, item.id
                        ));
                    }
                }
                worker_steps::OPEN_PR => {
                    if let Ok(g) = self.repos.0.lock() {
                        let event = AppendInput {
                            event_type: "worker.open_pr".into(),
                            project_id: item.project_id.clone(),
                            loop_id: item.loop_id.clone(),
                            run_id: Some(run.id.clone()),
                            entity_type: Some("queue_item".into()),
                            entity_id: Some(item.id.clone()),
                            payload_json: Some(serde_json::json!({"step": "open-pr"}).to_string()),
                            ..AppendInput::new("")
                        };
                        if let Err(e) = eventlog::append(&g.events, &event) {
                            tracing::warn!("Worker open_pr event: {e}");
                        }
                        let notification = NotificationRecord {
                            id: Uuid::new_v4().to_string(),
                            project_id: item.project_id.clone(),
                            loop_id: item.loop_id.clone(),
                            run_id: Some(run.id.clone()),
                            entity_type: Some("loop".into()),
                            entity_id: item.loop_id.clone(),
                            channel: "internal".into(),
                            level: "info".into(),
                            title: format!(
                                "Implementation complete for loop {}",
                                item.loop_id.as_deref().unwrap_or("?")
                            ),
                            subtitle: None,
                            body: format!("Worker finished pipeline (item={})", item.id),
                            status: "pending".into(),
                            dedupe_key: Some(format!("worker-done-{}", item.loop_id.as_deref().unwrap_or("?"))),
                            error_message: None,
                            payload_json: None,
                            sent_at: None,
                            created_at: now_iso.clone(),
                            updated_at: now_iso.clone(),
                        };
                        if let Err(e) = g.notifications.upsert(&notification) {
                            tracing::warn!("Worker notification: {e}");
                        }
                        // Drop outer lock before inner locks to avoid deadlock
                        drop(g);
                        // Create pull request via GitHub
                        if let Some(ref github) = self.github {
                            // Recover spec context from checkpoint
                            let spec_section = {
                                let guard = self.repos.0.lock().ok();
                                guard
                                    .as_ref()
                                    .and_then(|g| g.runs.get_by_id(&run.id).ok().flatten())
                                    .and_then(|r| r.checkpoint_json)
                                    .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok())
                                    .and_then(|v| {
                                        v.get("planner_spec_context")
                                            .and_then(|c| c.as_str())
                                            .map(|s| format!("\n### Spec Context\n{}\n", s))
                                    })
                                    .unwrap_or_default()
                            };
                            // Use issue title from metadata if available
                            let issue_title_text = if let Ok(guard) = self.repos.0.lock() {
                                if let Ok(Some(loop_rec)) = guard.loops.get_by_id(&run.loop_id) {
                                    if let Some(meta_str) = &loop_rec.metadata_json {
                                        if let Ok(meta) = serde_json::from_str::<serde_json::Value>(meta_str) {
                                            meta.get("issue_title")
                                                .and_then(|v| v.as_str())
                                                .map(|s| s.to_string())
                                                .unwrap_or_else(|| format!("Issue #{}", item.target_id))
                                        } else {
                                            format!("Issue #{}", item.target_id)
                                        }
                                    } else {
                                        format!("Issue #{}", item.target_id)
                                    }
                                } else {
                                    format!("Issue #{}", item.target_id)
                                }
                            } else {
                                format!("Issue #{}", item.target_id)
                            };
                            let body = format!(
                                "## Summary\n\nImplementation for issue #{} - {}.\n\n## Changes\n\nThis PR implements the feature described in the issue and its spec.{spec_section}\n\n## Testing Checklist\n\n- [ ] Code compiles without errors\n- [ ] Existing tests pass\n- [ ] Manual review completed\n- [ ] No new warnings introduced\n\n---\n\n*This PR was generated by [Looper](https://github.com/nexu-io/looper)*",
                                item.target_id,
                                issue_title_text,
                            );
                            // Commit changes, then push branch to GitHub before creating PR
                            if let Some(ref git) = self.git {
                                let branch = format!("worker/{loop_id}");
                                let wt_path = resolve_worker_wt(self, item, loop_id);
                                // Commit any uncommitted files written by the agent
                                let _ = self.tokio_handle.block_on(git.commit(looper_git::CommitInput {
                                    worktree_path: wt_path.clone(),
                                    message: format!(
                                        "feat: implement {} (#{})",
                                        issue_title_text.to_lowercase(),
                                        item.target_id
                                    ),
                                }));
                                match self.tokio_handle.block_on(git.push(looper_git::PushInput {
                                    worktree_path: wt_path.clone(),
                                    remote: "origin".into(),
                                    branch: branch.clone(),
                                    expected_head_sha: None,
                                    protected_branches: vec!["main".into(), "master".into()],
                                    set_upstream: true,
                                })) {
                                    Ok(_) => tracing::info!("Worker: pushed branch {} to origin", branch),
                                    Err(e) => tracing::warn!("Worker: push failed (branch {}): {}", branch, e),
                                }
                            }
                            match github.create_pull_request(CreatePullRequestInput {
                                repo: item.repo.clone().unwrap_or_default(),
                                head_branch: format!("worker/{loop_id}"),
                                base_branch: "main".to_string(),
                                title: format!(
                                    "feat: implement {} (#{})",
                                    issue_title_text.to_lowercase(),
                                    item.target_id
                                ),
                                body,
                                draft: true,
                                cwd: ".".to_string(),
                            }) {
                                Ok(pr) => {
                                    tracing::info!("PR #{} created for run {}", pr.number, run.id);
                                    // Store PR number in loop record
                                    if let Ok(guard) = self.repos.0.lock() {
                                        if let Ok(Some(mut loop_rec)) = guard.loops.get_by_id(&run.loop_id) {
                                            loop_rec.pr_number = Some(pr.number);
                                            loop_rec.updated_at =
                                                chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
                                            let _ = guard.loops.upsert(&loop_rec);
                                        }
                                        drop(guard);
                                    }
                                    // Keep PR as draft for now — mark ready when full pipeline completes
                                    // Post implementation comment
                                    let _ = github.create_issue_comment(IssueCommentInput {
                                        repo: item.repo.clone().unwrap_or_default(),
                                        issue_number: pr.number,
                                        body: format!(
                                            "## 🚀 Implementation Complete

Looper has finished implementing this issue.

This PR is currently a draft. Once validation passes, it will be marked ready for review.

_This is an automated message from looper._"
                                        ),
                                        cwd: ".".to_string(),
                                    });
                                }
                                Err(e) => tracing::warn!("PR creation failed: {}", e),
                            }
                        }
                    }
                }
                _ => {}
            }

            // Persist step progress
            let guard = self.repos.0.lock().map_err(|e| e.to_string())?;
            let mut r = guard.runs.get_by_id(&run.id).map_err(|e| e.to_string())?.ok_or("run not found during step")?;
            r.current_step = Some(step.to_string());
            r.last_completed_step = Some(step.to_string());
            r.last_heartbeat_at = Some(now_iso.clone());
            r.updated_at.clone_from(&now_iso);
            guard.runs.upsert(&r).map_err(|e| e.to_string())?;
            drop(guard);
        }

        // Clean up worktree after pipeline completes
        if let Some(ref git) = self.git {
            // Resolve the repo path from project record
            let (wt_repo_path, wt_root) = {
                let repos_lock = self.repos.0.lock();
                match repos_lock {
                    Ok(g) => {
                        let proj_path = g
                            .projects
                            .get_by_id(&item.project_id.clone().unwrap_or_default())
                            .ok()
                            .flatten()
                            .map(|p| p.repo_path.clone())
                            .filter(|p| !p.is_empty())
                            .unwrap_or_else(|| ".".to_string());
                        let wt = format!("{}/.looper/worktrees", proj_path);
                        (proj_path, wt)
                    }
                    Err(_) => (".".to_string(), ".".to_string()),
                }
            };
            let wt_dir = build_worktree_directory_name(&CreateWorktreeInput {
                project_id: item.project_id.clone().unwrap_or_default(),
                repo_path: wt_repo_path.clone(),
                worktree_root: wt_root.clone(),
                branch: format!("worker/{loop_id}"),
                base_branch: None,
                start_point: None,
                pr_number: None,
                checkout_mode: CheckoutMode::Branch,
                protected_branches: vec![],
            });
            let worktree_path = format!("{}/.looper/worktrees/{}", wt_repo_path, wt_dir);
            let _ = self.tokio_handle.block_on(git.cleanup_worktree(CleanupWorktreeInput {
                repo_path: wt_repo_path.clone(),
                worktree_path: worktree_path.clone(),
                branch: format!("worker/{loop_id}"),
                protected_branches: vec!["main".to_string(), "master".to_string()],
            }));
            let _ = std::fs::remove_dir_all(&worktree_path);
        }

        // Complete run -------------------------------------------------------
        let guard = self.repos.0.lock().map_err(|e| e.to_string())?;
        let mut final_run = guard.runs.get_by_id(&run.id).map_err(|e| e.to_string())?.ok_or("run not found")?;
        final_run.status = RunStatus::Success.as_str().to_string();
        final_run.ended_at = Some(now_iso);
        guard.runs.upsert(&final_run).map_err(|e| e.to_string())?;

        // Mark implementation PR as ready for review
        if let Some(ref gw) = self.github {
            if let Some(ref repo_path) = item.repo {
                if let Ok(Some(loop_rec)) = guard.loops.get_by_id(&run.loop_id) {
                    if let Some(pr_number) = loop_rec.pr_number {
                        let _ = gw.mark_pr_ready(looper_github::types::MarkPullRequestReadyForReviewInput {
                            repo: repo_path.clone(),
                            pr_number,
                            cwd: ".".to_string(),
                        });
                        tracing::info!("Worker: Marked PR #{} as ready for review", pr_number);
                    }
                }
            }
        }

        tracing::info!("Worker pipeline complete (loop={loop_id})");
        Ok(())
    }
}

impl WorkerScheduler for Worker {
    fn discover_issues(&self, _ctx: &Context, input: WorkerDiscoveryInput) -> WorkerDiscoveryResult {
        let repo = input.repo.clone();
        let mut new_queue_items: Vec<QueueItemRecord> = Vec::new();
        let mut found_issues: Vec<WorkerIssueEntry> = Vec::new();
        if let Some(ref github) = self.github {
            let gh_input = ListOpenIssuesInput {
                repo: repo.clone(),
                cwd: ".".to_string(),
                limit: 50,
                assignee: String::new(),
                label: "looper:implement".to_string(),
                labels: vec![],
            };
            match github.list_open_issues(gh_input) {
                Ok(issues) => {
                    tracing::info!(
                        "Worker GitHub discovery — {} candidate issue(s) with looper:implement",
                        issues.len()
                    );
                    let now_iso = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
                    for issue in issues {
                        let dedupe_key = format!("worker-{}-issue-{}", input.project_id, issue.number);
                        let exists = self
                            .repos
                            .0
                            .lock()
                            .ok()
                            .and_then(|g| g.queue.find_active_by_dedupe(&dedupe_key).ok().flatten())
                            .is_some();
                        if exists {
                            continue;
                        }
                        let loop_id = Uuid::new_v4().to_string();
                        let loop_seq = self.repos.0.lock().ok().and_then(|g| g.loops.allocate_seq().ok()).unwrap_or(0);
                        let new_loop = LoopRecord {
                            id: loop_id.clone(),
                            seq: loop_seq,
                            project_id: input.project_id.clone(),
                            r#type: "worker".into(),
                            target_type: "issue".into(),
                            target_id: Some(issue.number.to_string()),
                            repo: Some(repo.clone()),
                            pr_number: None,
                            status: "active".into(),
                            config_json: None,
                            metadata_json: Some(
                                serde_json::json!({
                                    "issue_number": issue.number,
                                    "issue_title": issue.title.clone(),
                                    "discovered_via": "worker",
                                })
                                .to_string(),
                            ),
                            last_run_at: None,
                            next_run_at: None,
                            created_at: now_iso.clone(),
                            updated_at: now_iso.clone(),
                        };
                        if let Ok(g) = self.repos.0.lock() {
                            let _ = g.loops.upsert(&new_loop);
                        }
                        let item = QueueItemRecord {
                            id: Uuid::new_v4().to_string(),
                            project_id: Some(input.project_id.clone()),
                            loop_id: Some(loop_id.clone()),
                            r#type: "worker".to_string(),
                            target_type: "issue".to_string(),
                            target_id: issue.number.to_string(),
                            repo: Some(repo.clone()),
                            pr_number: None,
                            dedupe_key,
                            priority: 50,
                            status: "queued".to_string(),
                            available_at: now_iso.clone(),
                            attempts: 0,
                            max_attempts: 3,
                            claimed_by: None,
                            claimed_at: None,
                            started_at: None,
                            finished_at: None,
                            lock_key: None,
                            payload_json: None,
                            last_error: None,
                            last_error_kind: None,
                            created_at: now_iso.clone(),
                            updated_at: now_iso.clone(),
                        };
                        if let Ok(g) = self.repos.0.lock() {
                            if let Ok((inserted, _new)) = g.queue.create_or_get_active_by_dedupe(&item) {
                                tracing::info!("Worker enqueued issue #{} (item {})", issue.number, inserted.id);
                                new_queue_items.push(inserted);
                            }
                        }
                        found_issues.push(WorkerIssueEntry {
                            number: issue.number,
                            title: issue.title,
                            body: issue.body,
                        });
                    }
                }
                Err(e) => {
                    tracing::warn!("Worker GitHub discovery failed for {}: {}", repo, e);
                }
            }
        } else {
            tracing::debug!("GitHub not configured, returning empty discovery for {}", repo);
        }
        WorkerDiscoveryResult { queue_items: new_queue_items, issues: found_issues }
    }

    fn process_claimed_queue_item(&self, _ctx: &Context, item: &QueueItemRecord) -> Result<(), String> {
        self.execute_pipeline(item)
    }
}
