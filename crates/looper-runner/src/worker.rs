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

use std::process::Command;

use chrono::Utc;
use uuid::Uuid;

use looper_agent::executor::ConfiguredExecutor;
use looper_git::{build_worktree_directory_name, Gateway as GitGateway};
use looper_git::types::{CheckoutMode, CleanupWorktreeInput, CreateWorktreeInput};
use looper_scheduler::scheduler::SendRepos;
use looper_scheduler::types::{
    Context, SchedulerConfig, WorkerDiscoveryInput, WorkerDiscoveryResult, WorkerIssueEntry,
    WorkerScheduler,
};
use looper_storage::eventlog;
use looper_storage::record::{
    AppendInput, NotificationRecord, QueueItemRecord, QueueMarkRetryInput, RunRecord,
};
use looper_types::RunStatus;

use looper_github::gateway::Gateway;
use looper_github::types::{
    CreatePullRequestInput, ListOpenIssuesInput, ListOpenPullRequestsInput, ViewPullRequestInput,
};

use crate::types::{worker_steps, SpecPRInfo, spec_labels};

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

impl Worker {
    pub fn new(
        config: &SchedulerConfig,
        repos: Arc<SendRepos>,
        github: Option<Arc<Gateway>>,
        tokio_handle: tokio::runtime::Handle,
        agent: Option<Arc<ConfiguredExecutor>>,
        git: Option<Arc<GitGateway>>,
    ) -> Self {
        Self {
            config: config.clone(),
            repos,
            github,
            tokio_handle,
            agent,
            git,
        }
    }

    /// Find the planner-created spec PR that references a given issue number.
    ///
    /// Returns `None` when the issue has no planner spec PR, or when the
    /// GitHub gateway is unavailable / the query fails.  Non-fatal — the
    /// worker falls back to implementing from the issue body directly.
    fn find_spec_pr_for_issue(
        &self,
        repo: &str,
        issue_number: i64,
    ) -> Option<looper_github::types::PullRequestDetail> {
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
            .filter(|pr| {
                pr.labels
                    .iter()
                    .any(|l| l == spec_labels::SPEC_REVIEWING || l == spec_labels::SPEC_READY)
            })
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

        let loop_id = item
            .loop_id
            .as_deref()
            .ok_or_else(|| "Worker queue item has no loop_id".to_string())?;

        // Create / resume run -------------------------------------------------
        let guard = self.repos.0.lock().map_err(|e| e.to_string())?;
        let run = match guard.runs.get_latest_by_loop_id(loop_id).map_err(|e| e.to_string())? {
            Some(run) if run.status == RunStatus::Running.as_str()
                || run.status == RunStatus::Queued.as_str() =>
            {
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
                        let input = looper_git::types::CreateWorktreeInput {
                            project_id: item.project_id.clone().unwrap_or_default(),
                            repo_path: ".".to_string(),
                            worktree_root: ".".to_string(),
                            branch: format!("worker/{loop_id}"),
                            base_branch: None,
                            start_point: None,
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
                        let input = looper_agent::executor::StartInput {
                            loop_id: run.loop_id.clone(),
                            current_step: Some("plan".to_string()),
                            last_completed_step: Some("prepare_worktree".to_string()),
                            checkpoint_json: None,
                            project_id: item.project_id.clone().unwrap_or_default(),
                            run_id: run.id.clone(),
                            working_directory: String::new(),
                            prompt: format!(
                                "Plan the implementation for this task in repo {}. Create PLAN.md with the implementation steps.",
                                item.repo.as_deref().unwrap_or("unknown")
                            ),
                        };
                        match self.tokio_handle.block_on(agent.start(input)) {
                            Ok(_) => tracing::info!("Agent plan started for run {}", run.id),
                            Err(e) => tracing::warn!("Agent plan failed: {}", e),
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
                                        pr_detail.number, issue_num, spec_path, info.phase
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
                            r.checkpoint_json = Some(
                                serde_json::json!({"planner_spec_context": spec_context})
                                    .to_string(),
                            );
                        }
                        let _ = guard.runs.upsert(&r);
                        drop(guard);
                    }

                    // Execute via agent
                    if let Some(ref agent) = self.agent {
                        let input = looper_agent::executor::StartInput {
                            loop_id: run.loop_id.clone(),
                            current_step: Some("execute".to_string()),
                            last_completed_step: Some("prepare_worktree".to_string()),
                            checkpoint_json: None,
                            project_id: item.project_id.clone().unwrap_or_default(),
                            run_id: run.id.clone(),
                            working_directory: String::new(),
                            prompt: if spec_context.is_empty() {
                                format!(
                                    "Execute the planned implementation for this task in repo {}. Write the actual code changes needed.",
                                    item.repo.as_deref().unwrap_or("unknown")
                                )
                            } else {
                                format!(
                                    "Execute the planned implementation for this task in repo {}. \
                                     Write the actual code changes needed.\n\n\
                                     Below is the spec context from the planner:\n{spec_context}",
                                    item.repo.as_deref().unwrap_or("unknown")
                                )
                            },
                        };
                        match self.tokio_handle.block_on(agent.start(input)) {
                            Ok(_) => tracing::info!("Agent execution started for run {}", run.id),
                            Err(e) => tracing::warn!("Agent execution failed: {}", e),
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

                    // Compute the worktree directory — same formula as PREPARE_WORKTREE and cleanup.
                    let wt_dir = build_worktree_directory_name(&CreateWorktreeInput {
                        project_id: item.project_id.clone().unwrap_or_default(),
                        repo_path: ".".to_string(),
                        worktree_root: ".".to_string(),
                        branch: format!("worker/{loop_id}"),
                        base_branch: None,
                        start_point: None,
                        pr_number: None,
                        checkout_mode: CheckoutMode::Branch,
                        protected_branches: vec![],
                    });
                    let worktree_path = format!("./{}", wt_dir);

                    // Run cargo build in the worktree directory to validate the change.
                    tracing::info!("Worker validate: running cargo build in {worktree_path}");
                    let build_output = Command::new("cargo")
                        .args(["build"])
                        .current_dir(&worktree_path)
                        .output()
                        .map_err(|e| format!("Failed to execute cargo build in worktree: {e}"))?;

                    if build_output.status.success() {
                        tracing::info!("Worker validate: cargo build succeeded for run {}", run.id);
                    } else {
                        let stderr = String::from_utf8_lossy(&build_output.stderr);
                        let stdout = String::from_utf8_lossy(&build_output.stdout);
                        tracing::warn!(
                            "Worker validate: cargo build FAILED for run {}:\nstdout:\n{}\nstderr:\n{}",
                            run.id, stdout, stderr,
                        );

                        // Mark the queue item for retry so it will be picked up again.
                        let guard = self.repos.0.lock().map_err(|e| e.to_string())?;
                        let attempts = item.attempts + 1;
                        let retry_at = Utc::now()
                            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                            .to_string();
                        if let Err(e) = guard.queue.mark_retry(&QueueMarkRetryInput {
                            id: item.id.clone(),
                            available_at: retry_at.clone(),
                            attempts,
                            error_message: Some(format!("cargo build failed: {}", stderr.lines().next().unwrap_or("unknown"))),
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
                            failed_run.error_message = Some(format!("cargo build failed: {}", stderr.lines().next().unwrap_or("unknown")));
                            failed_run.ended_at = Some(retry_at.clone());
                            failed_run.updated_at.clone_from(&retry_at);
                            if let Err(e) = guard.runs.upsert(&failed_run) {
                                tracing::warn!("Worker validate: failed to mark run as failed: {e}");
                            }
                        }

                        return Err(format!(
                            "Worker validate: cargo build failed for run {} (item {})",
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
                            title: format!("Implementation complete for loop {}", item.loop_id.as_deref().unwrap_or("?")),
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
                        // Create pull request via GitHub
                        if let Some(ref github) = self.github {
                            // Recover spec context from checkpoint
                            let spec_section = {
                                let guard = self.repos.0.lock().ok();
                                guard.as_ref().and_then(|g| {
                                    g.runs.get_by_id(&run.id).ok().flatten()
                                }).and_then(|r| r.checkpoint_json)
                                    .and_then(|json| {
                                        serde_json::from_str::<serde_json::Value>(&json).ok()
                                    })
                                    .and_then(|v| {
                                        v.get("planner_spec_context")
                                            .and_then(|c| c.as_str())
                                            .map(|s| format!("\n### Spec Context\n{}\n", s))
                                    })
                                    .unwrap_or_default()
                            };
                            let body = format!(
                                "Automated work loop execution.\n\n\
                                 Issue: #{}\n{spec_section}",
                                item.target_id,
                            );
                            match github.create_pull_request(CreatePullRequestInput {
                                repo: item.repo.clone().unwrap_or_default(),
                                head_branch: format!("worker/{loop_id}"),
                                base_branch: "main".to_string(),
                                title: format!("Auto: Work from loop {loop_id}"),
                                body,
                                cwd: ".".to_string(),
                            }) {
                                Ok(pr) => tracing::info!("PR #{} created for run {}", pr.number, run.id),
                                Err(e) => tracing::warn!("PR creation failed: {}", e),
                            }
                        }
                    }
                }
                _ => {}
            }

            // Persist step progress
            let guard = self.repos.0.lock().map_err(|e| e.to_string())?;
            let mut r = guard
                .runs
                .get_by_id(&run.id)
                .map_err(|e| e.to_string())?
                .ok_or("run not found during step")?;
            r.current_step = Some(step.to_string());
            r.last_completed_step = Some(step.to_string());
            r.last_heartbeat_at = Some(now_iso.clone());
            r.updated_at.clone_from(&now_iso);
            guard.runs.upsert(&r).map_err(|e| e.to_string())?;
            drop(guard);
        }

        // Clean up worktree after pipeline completes
        if let Some(ref git) = self.git {
            let wt_dir = build_worktree_directory_name(&CreateWorktreeInput {
                project_id: item.project_id.clone().unwrap_or_default(),
                repo_path: ".".to_string(),
                worktree_root: ".".to_string(),
                branch: format!("worker/{loop_id}"),
                base_branch: None,
                start_point: None,
                pr_number: None,
                checkout_mode: CheckoutMode::Branch,
                protected_branches: vec![],
            });
            let worktree_path = format!("./{}", wt_dir);
            let _ = self.tokio_handle.block_on(git.cleanup_worktree(
                CleanupWorktreeInput {
                    repo_path: ".".to_string(),
                    worktree_path: worktree_path.clone(),
                    branch: format!("worker/{loop_id}"),
                    protected_branches: vec!["main".to_string(), "master".to_string()],
                },
            ));
            let _ = std::fs::remove_dir_all(&worktree_path);
        }

        // Complete run -------------------------------------------------------
        let guard = self.repos.0.lock().map_err(|e| e.to_string())?;
        let mut final_run = guard
            .runs
            .get_by_id(&run.id)
            .map_err(|e| e.to_string())?
            .ok_or("run not found")?;
        final_run.status = RunStatus::Success.as_str().to_string();
        final_run.ended_at = Some(now_iso);
        guard.runs.upsert(&final_run).map_err(|e| e.to_string())?;

        tracing::info!("Worker pipeline complete (loop={loop_id})");
        Ok(())
    }
}

impl WorkerScheduler for Worker {
    fn discover_issues(
        &self,
        _ctx: &Context,
        input: WorkerDiscoveryInput,
    ) -> WorkerDiscoveryResult {
        let repo = input.repo.clone();
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
                    return WorkerDiscoveryResult {
                        issues: issues.into_iter().map(|issue| WorkerIssueEntry {
                            number: issue.number,
                            title: issue.title,
                            body: issue.body,
                        }).collect(),
                        ..Default::default()
                    };
                }
                Err(e) => {
                    tracing::warn!("GitHub issue discovery failed for {}: {}", repo, e);
                }
            }
        }
        tracing::debug!("GitHub not configured, returning empty discovery for {}", repo);
        WorkerDiscoveryResult::default()
    }

    fn process_claimed_queue_item(
        &self,
        _ctx: &Context,
        item: &QueueItemRecord,
    ) -> Result<(), String> {
        self.execute_pipeline(item)
    }
}
