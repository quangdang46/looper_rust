//! Planner runner — implements the [`PlannerScheduler`] trait.
//!
//! The Planner is the first role in the Looper workflow pipeline. It
//! discovers unplanned issues, creates a loop + worktree, writes a
//! specification, publishes a PR, and notifies stakeholders.
//!
//! **Step pipeline** (5 steps):
//!   1. `discover-issues`   — identify candidate work items
//!   2. `prepare-worktree`  — create/restore a git worktree for the loop
//!   3. `write-spec`        — produce the plan / specification document
//!   4. `publish`           — open a PR (or push to a tracking branch)
//!   5. `notify`            — send notification that planning is complete

use std::sync::Arc;

use chrono::Utc;
use looper_github::types::{IssueCommentInput, IssueLabelsInput, ListOpenIssuesInput};
use uuid::Uuid;

use looper_agent::executor::ConfiguredExecutor;
use looper_git::Gateway as GitGateway;
use looper_git::{build_worktree_directory_name, CreateWorktreeInput};
use looper_scheduler::scheduler::SendRepos;
use looper_scheduler::types::{
    Context, PlannerDiscoveryInput, PlannerDiscoveryResult, PlannerProcessInput, PlannerProcessResult,
    PlannerScheduler, SchedulerConfig,
};
use looper_storage::eventlog;
use looper_storage::record::{AppendInput, LoopRecord, NotificationRecord, QueueItemRecord, RunRecord, WorktreeRecord};
use looper_types::RunStatus;

use crate::types::planner_steps;

/// Planner runner state machine.
///
/// Stores a clone of the scheduler config and an Arc to the thread-safe
/// repository wrapper so that [`process_claimed_queue_item`] can advance
/// the run through its step pipeline.
pub struct Planner {
    pub config: SchedulerConfig,
    pub repos: Arc<SendRepos>,
    pub github: Option<Arc<looper_github::Gateway>>,
    pub agent: Option<Arc<ConfiguredExecutor>>,
    pub git: Option<Arc<GitGateway>>,
    pub tokio_handle: tokio::runtime::Handle,
}

// SAFETY: SendRepos wraps Mutex<Repositories>, making it Send+Sync.
// Planner contains only Send+Sync types.
unsafe impl Send for Planner {}
unsafe impl Sync for Planner {}

impl Planner {
    pub fn new(
        config: &SchedulerConfig,
        repos: Arc<SendRepos>,
        github: Option<Arc<looper_github::Gateway>>,
        agent: Option<Arc<ConfiguredExecutor>>,
        git: Option<Arc<GitGateway>>,
        tokio_handle: tokio::runtime::Handle,
    ) -> Self {
        Self { config: config.clone(), repos, github, agent, git, tokio_handle }
    }
}

impl PlannerScheduler for Planner {
    fn discover_issues(&self, _ctx: &Context, input: PlannerDiscoveryInput) -> PlannerDiscoveryResult {
        // Planner discovery scans for un-queued issues that need planning.
        tracing::debug!("Planner discover_issues — scanning for unplanned work items in {repo}", repo = input.repo);

        let mut new_queue_items: Vec<QueueItemRecord> = Vec::new();

        // GitHub-powered issue discovery — creates a loop + queue item
        // for every open issue that carries the `looper:plan` label.
        if let Some(ref gw) = self.github {
            if !input.repo.is_empty() {
                match gw.list_open_issues(ListOpenIssuesInput {
                    repo: input.repo.clone(),
                    cwd: ".".to_string(),
                    limit: 50,
                    assignee: String::new(),
                    label: "looper:plan".to_string(),
                    labels: vec![],
                }) {
                    Ok(issues) => {
                        tracing::info!(
                            "Planner GitHub discovery — {} candidate issue(s) with looper:plan",
                            issues.len()
                        );
                        let now_iso = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();

                        for issue in issues {
                            // Skip issues that have already been
                            // planned in a previous pipeline run.
                            if issue.labels.iter().any(|l| l == "looper/planned") {
                                tracing::debug!(
                                    "Planner skipping issue #{} (already labeled looper/planned)",
                                    issue.number
                                );
                                continue;
                            }
                            let dedupe_key = format!("planner-{}-issue-{}", input.project_id, issue.number);

                            // Dedup: if a queue item with this dedupe_key
                            // already exists, skip.
                            let exists = match self.repos.0.lock() {
                                Ok(g) => g.queue.find_active_by_dedupe(&dedupe_key).ok().flatten().is_some(),
                                Err(_) => false,
                            };
                            if exists {
                                tracing::debug!("Planner skipping issue #{} (already queued)", issue.number);
                                continue;
                            }

                            // Create a loop for this issue.
                            let loop_id = Uuid::new_v4().to_string();
                            let loop_seq = self.repos.0.lock().ok().and_then(|g| g.loops.allocate_seq().ok());
                            if let Some(seq) = loop_seq {
                                let new_loop = LoopRecord {
                                    id: loop_id.clone(),
                                    seq,
                                    project_id: input.project_id.clone(),
                                    r#type: "planner".into(),
                                    target_type: "issue".into(),
                                    target_id: Some(issue.number.to_string()),
                                    repo: Some(input.repo.clone()),
                                    pr_number: None,
                                    status: "active".into(),
                                    config_json: None,
                                    metadata_json: Some(
                                        serde_json::json!({
                                            "issue_number": issue.number,
                                            "issue_title": issue.title.clone(),
                                            "discovered_via": "planner",
                                        })
                                        .to_string(),
                                    ),
                                    last_run_at: None,
                                    next_run_at: None,
                                    created_at: now_iso.clone(),
                                    updated_at: now_iso.clone(),
                                };
                                if let Ok(g) = self.repos.0.lock() {
                                    if let Err(e) = g.loops.upsert(&new_loop) {
                                        tracing::warn!("Planner loop upsert failed for issue #{}: {e}", issue.number);
                                        continue;
                                    }
                                }
                            }

                            // Create a queue item pointing at the loop.
                            let queue_item = QueueItemRecord {
                                id: Uuid::new_v4().to_string(),
                                project_id: Some(input.project_id.clone()),
                                loop_id: Some(loop_id.clone()),
                                r#type: "planner".into(),
                                target_type: "issue".into(),
                                target_id: issue.number.to_string(),
                                repo: Some(input.repo.clone()),
                                pr_number: None,
                                dedupe_key: dedupe_key.clone(),
                                priority: 10,
                                status: "queued".into(),
                                available_at: now_iso.clone(),
                                attempts: 0,
                                max_attempts: 3,
                                claimed_by: None,
                                claimed_at: None,
                                started_at: None,
                                finished_at: None,
                                lock_key: Some(format!("planner-{}-issue-{}", input.project_id, issue.number)),
                                payload_json: Some(
                                    serde_json::json!({
                                        "issue_number": issue.number,
                                        "issue_title": issue.title.clone(),
                                        "url": issue.url.clone(),
                                    })
                                    .to_string(),
                                ),
                                last_error: None,
                                last_error_kind: None,
                                created_at: now_iso.clone(),
                                updated_at: now_iso.clone(),
                            };
                            match self.repos.0.lock() {
                                Ok(g) => match g.queue.upsert_active_by_dedupe_or_get_existing(&queue_item) {
                                    Ok((inserted, _is_new)) => {
                                        tracing::info!(
                                            "Planner enqueued issue #{} (item {})",
                                            issue.number,
                                            inserted.id
                                        );
                                        new_queue_items.push(inserted);
                                    }
                                    Err(e) => {
                                        tracing::warn!("Planner queue upsert failed for issue #{}: {e}", issue.number)
                                    }
                                },
                                Err(e) => {
                                    tracing::warn!("Planner lock failed: {e}");
                                }
                            }
                        }
                    }
                    Err(e) => tracing::warn!("Planner GitHub issue discovery failed: {e}"),
                }
            }
        }

        // Re-list existing queued planner items (to include both new
        // and re-discovered items) so the scheduler tracks them.
        let existing_planner = match self.repos.0.lock() {
            Ok(g) => g
                .queue
                .list_by_statuses(&["queued".into(), "running".into()])
                .ok()
                .unwrap_or_default()
                .into_iter()
                .filter(|item| item.r#type == "planner")
                .collect::<Vec<_>>(),
            Err(_) => Vec::new(),
        };

        // Merge: prefer newly-discovered items, then fill with existing.
        let mut merged = new_queue_items;
        for item in existing_planner {
            if !merged.iter().any(|q| q.id == item.id) {
                merged.push(item);
            }
        }
        tracing::debug!("Planner discover_issues done — {} planner item(s) tracked", merged.len());

        PlannerDiscoveryResult { queue_items: merged, created_loops: vec![] }
    }

    fn process_claimed_queue_item(&self, ctx: &Context, input: PlannerProcessInput) -> PlannerProcessResult {
        let item = &input.item;
        let now_iso = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();

        // 1. Resolve loop ---------------------------------------------------
        let loop_id = match item.loop_id.as_deref() {
            Some(id) => id.to_string(),
            None => {
                tracing::error!("Planner queue item {} has no loop_id", item.id);
                return PlannerProcessResult;
            }
        };
        let project_id = item.project_id.as_deref().unwrap_or("unknown").to_string();

        // Resolve local filesystem path (for git operations) from the
        // project record. item.repo carries the GitHub URL (for `gh`),
        // but git worktree operations need a local clone path.
        let local_path = match self.repos.0.lock() {
            Ok(g) => g.projects.get_by_id(&project_id).ok().flatten().map(|p| p.repo_path.clone()).unwrap_or_default(),
            Err(_) => String::new(),
        };

        // 2. Lock repos and get the latest run for this loop -----------------
        let guard = match self.repos.0.lock() {
            Ok(g) => g,
            Err(e) => {
                tracing::error!("Planner repo lock poisoned: {e}");
                return PlannerProcessResult;
            }
        };

        let latest_run = match guard.runs.get_latest_by_loop_id(&loop_id) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("Planner get_latest_by_loop_id({loop_id}): {e}");
                return PlannerProcessResult;
            }
        };

        let run = match latest_run {
            Some(run) if run.status == RunStatus::Running.as_str() => {
                // Resume an existing running run
                run
            }
            Some(run) if run.status == RunStatus::Queued.as_str() => {
                // Start a queued run
                let mut updated = run.clone();
                updated.status = RunStatus::Running.as_str().to_string();
                updated.current_step = Some(planner_steps::DISCOVER_ISSUES.to_string());
                updated.started_at.clone_from(&now_iso);
                updated.updated_at.clone_from(&now_iso);
                if let Err(e) = guard.runs.upsert(&updated) {
                    tracing::error!("Planner upsert run start: {e}");
                    return PlannerProcessResult;
                }
                // Reload
                match guard.runs.get_by_id(&run.id) {
                    Ok(Some(r)) => r,
                    _ => return PlannerProcessResult,
                }
            }
            _ => {
                // Create a new run
                let new_run = RunRecord {
                    id: Uuid::new_v4().to_string(),
                    loop_id: loop_id.clone(),
                    status: RunStatus::Running.as_str().to_string(),
                    current_step: Some(planner_steps::DISCOVER_ISSUES.to_string()),
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
                if let Err(e) = guard.runs.upsert(&new_run) {
                    tracing::error!("Planner create run: {e}");
                    return PlannerProcessResult;
                }
                new_run
            }
        };
        drop(guard);

        // Look up issue details for the PR title / agent prompt
        let issue_number: i64 = item.target_id.parse().unwrap_or(0);
        let issue_title = match self.repos.0.lock() {
            Ok(g) => match g.loops.get_by_id(&loop_id) {
                Ok(Some(l)) => l
                    .metadata_json
                    .as_deref()
                    .and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok())
                    .and_then(|v| v.get("issue_title").and_then(|t| t.as_str()).map(String::from))
                    .unwrap_or_else(|| format!("Issue #{}", issue_number)),
                _ => format!("Issue #{}", issue_number),
            },
            Err(_) => format!("Issue #{}", issue_number),
        };

        // 3. Execute step pipeline -------------------------------------------
        let steps = planner_steps::ALL;
        let start_idx = run
            .last_completed_step
            .as_ref()
            .and_then(|lcs| steps.iter().position(|s| s == lcs))
            .map(|p| p + 1)
            .unwrap_or(0);

        for &step in &steps[start_idx..] {
            if ctx.is_cancelled() {
                tracing::info!("Planner pipeline cancelled at step {step}");
                break;
            }

            tracing::info!("Planner step: {step} (loop={loop_id})");

            // Execute step — each arm performs real DB operations
            // relevant to the step, while async external calls (agent,
            // GitHub, git) are attempted via tokio::runtime::Handle
            // when available.
            match step {
                planner_steps::DISCOVER_ISSUES => {
                    // Record a discovery event in the event log
                    if let Ok(g) = self.repos.0.lock() {
                        let event = AppendInput {
                            event_type: "planner.discover_issues".into(),
                            project_id: Some(project_id.clone()),
                            loop_id: Some(loop_id.clone()),
                            run_id: Some(run.id.clone()),
                            entity_type: Some("queue_item".into()),
                            entity_id: Some(item.id.clone()),
                            payload_json: Some(
                                serde_json::json!({
                                    "step": "discover-issues",
                                    "loop_id": loop_id,
                                })
                                .to_string(),
                            ),
                            ..AppendInput::new("")
                        };
                        if let Err(e) = eventlog::append(&g.events, &event) {
                            tracing::warn!("Planner event log: {e}");
                        }
                    }
                }

                planner_steps::PREPARE_WORKTREE => {
                    // Pre-compute the worktree physical path for both
                    // the DB record and the git gateway.
                    let wt_dir = looper_git::build_worktree_directory_name(&looper_git::CreateWorktreeInput {
                        project_id: project_id.clone(),
                        repo_path: String::new(),
                        worktree_root: String::new(),
                        branch: format!("planner/{loop_id}"),
                        base_branch: None,
                        start_point: None,
                        pr_number: None,
                        checkout_mode: looper_git::CheckoutMode::Branch,
                        protected_branches: vec![],
                    });
                    // Use CWD for filesystem operations. item.repo is the GitHub
                    // owner/repo identifier (quangdang46/test-looper) and is NOT a
                    // valid filesystem path. The daemon's CWD is the checkout dir.
                    let wt_base: String = if !local_path.is_empty() {
                        local_path.clone()
                    } else if let Ok(cwd) = std::env::current_dir() {
                        cwd.to_string_lossy().to_string()
                    } else {
                        ".".to_string()
                    };
                    let worktree_abs_path = format!("{}/.looper/worktrees/{wt_dir}", wt_base);
                    // Create a worktree record so downstream phases
                    // know which branch / worktree this loop uses.
                    if let Ok(g) = self.repos.0.lock() {
                        let worktree = WorktreeRecord {
                            id: Uuid::new_v4().to_string(),
                            project_id: project_id.clone(),
                            repo_path: wt_base.clone(),
                            worktree_path: worktree_abs_path.clone(),
                            branch: format!("planner/{loop_id}"),
                            base_branch: None,
                            status: "created".into(),
                            head_sha: None,
                            metadata_json: Some(
                                serde_json::json!({
                                    "loop_id": loop_id,
                                    "step": "prepare-worktree",
                                })
                                .to_string(),
                            ),
                            created_at: now_iso.clone(),
                            updated_at: now_iso.clone(),
                            cleaned_at: None,
                        };
                        if let Err(e) = g.worktrees.upsert(&worktree) {
                            tracing::warn!("Planner worktree upsert: {e}");
                        }
                    }
                    // When a git gateway is available, create the worktree on disk
                    if let Some(ref git) = self.git {
                        let branch_name = format!("planner/{loop_id}");
                        let effective_path = if !local_path.is_empty() {
                            local_path.clone()
                        } else if let Ok(cwd) = std::env::current_dir() {
                            cwd.to_string_lossy().to_string()
                        } else {
                            ".".to_string()
                        };
                        let branch = branch_name.clone();
                        let wt_input = looper_git::CreateWorktreeInput {
                            project_id: project_id.clone(),
                            repo_path: effective_path.clone(),
                            worktree_root: format!("{}/.looper/worktrees", effective_path),
                            branch,
                            base_branch: Some("main".to_string()),
                            start_point: Some("main".to_string()),
                            pr_number: None,
                            checkout_mode: looper_git::CheckoutMode::Branch,
                            protected_branches: vec!["main".to_string(), "master".to_string()],
                        };
                        let _ = self.tokio_handle.block_on(git.create_worktree(wt_input));
                    }
                }

                planner_steps::WRITE_SPEC => {
                    // Record a checkpoint with spec metadata.
                    // In production this would call the agent to write
                    // the plan document.
                    if let Ok(g) = self.repos.0.lock() {
                        let event = AppendInput {
                            event_type: "planner.write_spec".into(),
                            project_id: Some(project_id.clone()),
                            loop_id: Some(loop_id.clone()),
                            run_id: Some(run.id.clone()),
                            entity_type: Some("run".into()),
                            entity_id: Some(run.id.clone()),
                            payload_json: Some(
                                serde_json::json!({
                                    "step": "write-spec",
                                    "status": "spec_written",
                                })
                                .to_string(),
                            ),
                            ..AppendInput::new("")
                        };
                        if let Err(e) = eventlog::append(&g.events, &event) {
                            tracing::warn!("Planner spec event: {e}");
                        }
                    }
                    // When an agent executor is available, start it to write the spec
                    if let Some(ref agent) = self.agent {
                        let worktree_dir = build_worktree_directory_name(&CreateWorktreeInput {
                            project_id: project_id.clone(),
                            repo_path: String::new(),
                            worktree_root: String::new(),
                            branch: format!("planner/{loop_id}"),
                            base_branch: None,
                            start_point: None,
                            pr_number: None,
                            checkout_mode: looper_git::CheckoutMode::Branch,
                            protected_branches: vec![],
                        });
                        let effective_path = if !local_path.is_empty() {
                            local_path.clone()
                        } else if let Ok(cwd) = std::env::current_dir() {
                            cwd.to_string_lossy().to_string()
                        } else {
                            ".".to_string()
                        };
                        let worktree_abs_path = format!("{}/.looper/worktrees/{}", effective_path, worktree_dir);
                        let input = looper_agent::executor::StartInput {
                            loop_id: run.loop_id.clone(),
                            current_step: Some(planner_steps::WRITE_SPEC.to_string()),
                            last_completed_step: Some(planner_steps::DISCOVER_ISSUES.to_string()),
                            checkpoint_json: None,
                            project_id: project_id.clone(),
                            run_id: run.id.clone(),
                            working_directory: worktree_abs_path.clone(),
                            prompt: format!("Write a specification for issue #{issue_number}: {issue_title}\n\nAnalyze the issue and produce a structured spec document in markdown that contains the following sections in order. Use the exact section headings shown below:\n\n---\n## Objective\nA concise statement of what this change achieves.\n\n## Implementation Plan\nStep-by-step breakdown of the changes to make, in order of dependency.\n\n## Files to Change\nList every file that needs modification, creation, or deletion, with a brief note on what changes each file requires.\n\n## Risks\nPotential pitfalls, regressions, or edge cases to watch out for during implementation.\n\n## Acceptance Criteria\nConcrete, testable conditions that must be true after implementation (e.g., \"All existing tests pass\", \"New test coverage for edge case X\", \"The CLI reports Y when Z\").\n---\n\nAlso include a line like:\n  Spec: specs/{issue_number}-spec/spec.md\nat the end of the file so downstream tooling can locate the spec.\n\nWrite the spec to a file called `specs/{issue_number}-spec/spec.md` in the repo root ({worktree_abs_path}).\n\nIMPORTANT: Actually create the file. Use the `write` tool to write specs/{issue_number}-spec/spec.md with the full spec content

The spec file path MUST be included in the PR body as:
  specPath: specs/{issue_number}-spec/spec.md."),
                        };
                        match self.tokio_handle.block_on(agent.start(input)) {
                            Ok(exec) => {
                                tracing::info!("Planner agent started for loop {loop_id}, waiting for completion...");
                                match self.tokio_handle.block_on(exec.wait()) {
                                    Ok(result) => {
                                        tracing::info!("Planner agent completed for loop {loop_id}");
                                        // The agent may have written the spec file directly to disk
                                        // (when running with --dangerously-skip-permissions + --print).
                                        // Check the disk first, then fall back to stdout.
                                        let spec_dir = format!("{worktree_abs_path}/specs/{issue_number}-spec");
                                        let spec_path = format!("{spec_dir}/spec.md");
                                        let spec_on_disk =
                                            std::fs::read_to_string(&spec_path).ok().filter(|s| !s.trim().is_empty());
                                        if spec_on_disk.is_some() {
                                            tracing::info!(
                                                "Planner found spec on disk at {spec_path} for loop {loop_id}"
                                            );
                                        } else {
                                            // Agent didn't write file — save stdout/summary as spec
                                            let spec_content = if !result.stdout.trim().is_empty() {
                                                result.stdout
                                            } else if !result.summary.trim().is_empty() {
                                                result.summary
                                            } else {
                                                String::new()
                                            };
                                            if !spec_content.is_empty() {
                                                let _ = std::fs::create_dir_all(&spec_dir);
                                                match std::fs::write(&spec_path, &spec_content) {
                                                    Ok(_) => tracing::info!(
                                                        "Planner saved spec to {spec_path} for loop {loop_id}"
                                                    ),
                                                    Err(e) => tracing::warn!("Planner failed to save spec: {e}"),
                                                }
                                            }
                                        }
                                    }
                                    Err(e) => tracing::warn!("Planner agent execution failed for loop {loop_id}: {e}"),
                                }
                            }
                            Err(e) => tracing::warn!("Planner agent failed to start for loop {loop_id}: {e}"),
                        }
                    }
                }

                planner_steps::PUBLISH => {
                    // Log the publish intent
                    if let Ok(g) = self.repos.0.lock() {
                        let event = AppendInput {
                            event_type: "planner.publish".into(),
                            project_id: Some(project_id.clone()),
                            loop_id: Some(loop_id.clone()),
                            run_id: Some(run.id.clone()),
                            entity_type: Some("queue_item".into()),
                            entity_id: Some(item.id.clone()),
                            payload_json: Some(
                                serde_json::json!({
                                    "step": "publish",
                                    "target_id": item.target_id,
                                })
                                .to_string(),
                            ),
                            ..AppendInput::new("")
                        };
                        if let Err(e) = eventlog::append(&g.events, &event) {
                            tracing::warn!("Planner publish event: {e}");
                        }
                    }
                    // When a git gateway is available, commit + push the
                    // branch so `gh pr create` has commits to reference.
                    if let Some(ref git) = self.git {
                        let effective_path = if !local_path.is_empty() {
                            local_path.clone()
                        } else if let Ok(cwd) = std::env::current_dir() {
                            cwd.to_string_lossy().to_string()
                        } else {
                            ".".to_string()
                        };
                        let branch = format!("planner/{loop_id}");
                        let wt_input = looper_git::CreateWorktreeInput {
                            project_id: project_id.clone(),
                            repo_path: String::new(),
                            worktree_root: String::new(),
                            branch: branch.clone(),
                            base_branch: None,
                            start_point: None,
                            pr_number: None,
                            checkout_mode: looper_git::CheckoutMode::Branch,
                            protected_branches: vec![],
                        };
                        let worktree_dir_name = looper_git::build_worktree_directory_name(&wt_input);
                        let worktree_path = format!("{}/.looper/worktrees/{}", effective_path, worktree_dir_name);
                        // Commit the spec document that the agent wrote
                        // directly from the existing worktree (no need to
                        // remove and recreate).
                        self.tokio_handle.block_on(async {
                            if let Err(e) = git
                                .commit(looper_git::CommitInput {
                                    worktree_path: worktree_path.clone(),
                                    message: format!("[{issue_number}] {issue_title}"),
                                })
                                .await
                            {
                                tracing::warn!("Planner commit (non-fatal): {e}");
                            }
                            if let Err(e) = git
                                .push(looper_git::PushInput {
                                    worktree_path: worktree_path.clone(),
                                    remote: "origin".into(),
                                    branch: branch.clone(),
                                    expected_head_sha: None,
                                    protected_branches: vec!["main".into(), "master".into()],
                                    set_upstream: true,
                                })
                                .await
                            {
                                tracing::warn!("Planner push (non-fatal): {e}");
                            }
                        });
                    }
                    // When GitHub is available, create a spec pull request
                    if let Some(ref gw) = self.github {
                        if let Some(ref repo_path) = item.repo {
                            if !repo_path.is_empty() {
                                // Close any existing open spec PRs for this issue to avoid duplicates
                                if let Ok(open_prs) =
                                    gw.list_open_pull_requests(looper_github::types::ListOpenPullRequestsInput {
                                        repo: repo_path.clone(),
                                        cwd: ".".to_string(),
                                        limit: 50,
                                        label: String::new(),
                                        labels: vec![],
                                        author: String::new(),
                                        base_ref_name: String::new(),
                                        timeout: None,
                                    })
                                {
                                    for pr in &open_prs {
                                        if pr.title.starts_with(&format!("[{}]", issue_number)) {
                                            tracing::info!(
                                                "Planner closing old PR #{} for issue #{}",
                                                pr.number,
                                                issue_number
                                            );
                                            let _ =
                                                gw.close_pull_request(looper_github::types::ClosePullRequestInput {
                                                    repo: repo_path.clone(),
                                                    pr_number: pr.number,
                                                    cwd: ".".to_string(),
                                                });
                                            // Also cancel any old loops referencing this PR
                                            if let Ok(guard) = self.repos.0.lock() {
                                                if let Ok(loops) = guard.loops.list() {
                                                    for old_loop in &loops {
                                                        if old_loop.pr_number == Some(pr.number)
                                                            && !matches!(
                                                                old_loop.status.as_str(),
                                                                "completed" | "failed" | "cancelled" | "terminated"
                                                            )
                                                        {
                                                            let mut updated = old_loop.clone();
                                                            updated.status = "cancelled".to_string();
                                                            updated.updated_at = chrono::Utc::now()
                                                                .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                                                                .to_string();
                                                            let _ = guard.loops.upsert(&updated);
                                                            tracing::info!(
                                                                "Cancelled old loop {} for PR #{}",
                                                                old_loop.id,
                                                                pr.number
                                                            );
                                                        }
                                                    }
                                                }
                                                drop(guard);
                                            }
                                        }
                                    }
                                }
                                let body = format!(
                                    "## Spec: {}\n\n**Issue**: #{} | **Repository**: {} | **Status**: `planning`\n\n---\n\n## Objective\n\nThe planning spec for issue #{} has been written.\n\n## Spec Location\n\n`specs/{}-spec/spec.md`\n\nSpec: specs/{}-spec/spec.md\n\n## Next Steps\n\n1. Review the spec in the `specs/` directory of this PR\n2. The reviewer will check the spec for completeness\n3. Once approved, implementation will begin automatically\n\n---\n\n*Generated by [Looper](https://github.com/nexu-io/looper)*",
                                    issue_title,
                                    issue_number,
                                    repo_path,
                                    issue_number,
                                    issue_number,
                                    issue_number
                                );
                                match gw.create_pull_request(looper_github::types::CreatePullRequestInput {
                                    repo: repo_path.clone(),
                                    head_branch: format!("planner/{loop_id}"),
                                    base_branch: "main".to_string(),
                                    title: format!(
                                        "docs: add spec for {} (#{})",
                                        issue_title.to_lowercase(),
                                        issue_number
                                    ),
                                    body,
                                    draft: true,
                                    cwd: ".".to_string(),
                                }) {
                                    Ok(result) => {
                                        tracing::info!(
                                            "Planner created draft PR #{} for loop {loop_id}",
                                            result.number
                                        );
                                        // Mark the PR as being in spec-review phase
                                        let _ = gw.add_issue_labels(IssueLabelsInput {
                                            repo: repo_path.clone(),
                                            issue_number: result.number,
                                            labels: vec!["looper:spec-reviewing".into()],
                                            cwd: ".".to_string(),
                                        });
                                        // Mark PR as ready for review when pipeline completes (done in cleanup)
                                        // Post spec content as a PR comment for easy review
                                        // Spec was written to specs/{issue_number}-spec/spec.md in the worktree
                                        // Try reading from the local clone's worktree directory
                                        // Resolve worktree path from project record
                                        let spec_repo_path = if let Ok(guard) = self.repos.0.lock() {
                                            guard
                                                .projects
                                                .get_by_id(&project_id)
                                                .ok()
                                                .flatten()
                                                .map(|p| p.repo_path.clone())
                                                .filter(|p| !p.is_empty())
                                                .unwrap_or_else(|| ".".to_string())
                                        } else {
                                            ".".to_string()
                                        };
                                        let spec_path = format!("{spec_repo_path}/.looper/worktrees/planner-{loop_id}/specs/{issue_number}-spec/spec.md");
                                        if let Ok(spec_content) = std::fs::read_to_string(&spec_path) {
                                            let comment = format!(
                                                "## 📋 Spec: {issue_title}

{}",
                                                spec_content
                                            );
                                            let _ = gw.create_issue_comment(IssueCommentInput {
                                                repo: repo_path.clone(),
                                                issue_number: result.number,
                                                body: comment,
                                                cwd: ".".to_string(),
                                            });
                                            tracing::info!("Planner posted spec comment to PR #{}", result.number);
                                        }
                                    }
                                    Err(e) => tracing::warn!("Planner create PR failed: {e}"),
                                }
                            }
                        }
                    }
                }

                planner_steps::NOTIFY => {
                    // Create a notification record.
                    if let Ok(g) = self.repos.0.lock() {
                        let notification = NotificationRecord {
                            id: Uuid::new_v4().to_string(),
                            project_id: Some(project_id.clone()),
                            loop_id: Some(loop_id.clone()),
                            run_id: Some(run.id.clone()),
                            entity_type: Some("loop".into()),
                            entity_id: Some(loop_id.clone()),
                            channel: "internal".into(),
                            level: "info".into(),
                            title: format!("Planning complete for loop {loop_id}"),
                            subtitle: None,
                            body: format!("Planner finished pipeline for loop {loop_id} (item={})", item.id),
                            status: "pending".into(),
                            dedupe_key: Some(format!("planner-done-{loop_id}")),
                            error_message: None,
                            payload_json: None,
                            sent_at: None,
                            created_at: now_iso.clone(),
                            updated_at: now_iso.clone(),
                        };
                        if let Err(e) = g.notifications.upsert(&notification) {
                            tracing::warn!("Planner notification: {e}");
                        }
                    }
                    // When GitHub is available, add a label to track planned items
                    if let Some(ref gw) = self.github {
                        if let Some(ref repo_path) = item.repo {
                            // Label the original issue (target_id) so it
                            // is excluded from future looper:plan discovery.
                            if let Ok(issue_num) = item.target_id.parse::<i64>() {
                                let _ = gw.add_issue_labels(IssueLabelsInput {
                                    repo: repo_path.clone(),
                                    issue_number: issue_num,
                                    labels: vec!["looper/planned".into()],
                                    cwd: ".".to_string(),
                                });
                            }
                        }
                    }
                }

                _ => {
                    tracing::warn!("Planner unknown step: {step}");
                }
            }

            // Record step progress
            let guard = match self.repos.0.lock() {
                Ok(g) => g,
                Err(_) => break,
            };
            let mut updated = match guard.runs.get_by_id(&run.id) {
                Ok(Some(r)) => r,
                _ => break,
            };
            updated.current_step = Some(step.to_string());
            updated.last_completed_step = Some(step.to_string());
            updated.last_heartbeat_at = Some(now_iso.clone());
            updated.updated_at.clone_from(&now_iso);
            if let Err(e) = guard.runs.upsert(&updated) {
                tracing::error!("Planner record_step {step}: {e}");
                drop(guard);
                break;
            }
            drop(guard);
        }

        // Clean up the worktree after the pipeline completes so
        // worktree directories don't accumulate on disk.
        if let Some(ref git) = self.git {
            let wt_dir = build_worktree_directory_name(&CreateWorktreeInput {
                project_id: project_id.clone(),
                repo_path: String::new(),
                worktree_root: String::new(),
                branch: format!("planner/{loop_id}"),
                base_branch: None,
                start_point: None,
                pr_number: None,
                checkout_mode: looper_git::CheckoutMode::Branch,
                protected_branches: vec![],
            });
            let effective_path = if !local_path.is_empty() {
                local_path.clone()
            } else if let Ok(cwd) = std::env::current_dir() {
                cwd.to_string_lossy().to_string()
            } else {
                ".".to_string()
            };
            let worktree_path = format!("{}/.looper/worktrees/{}", effective_path, wt_dir);
            let _ = self.tokio_handle.block_on(git.cleanup_worktree(looper_git::types::CleanupWorktreeInput {
                repo_path: effective_path.clone(),
                worktree_path: worktree_path.clone(),
                branch: format!("planner/{loop_id}"),
                protected_branches: vec!["main".to_string(), "master".to_string()],
            }));
            // Belt-and-suspenders: if git worktree remove didn't clean
            // the dir (e.g. it was never registered), rm -rf it directly.
            let _ = std::fs::remove_dir_all(&worktree_path);
        }

        // 4. Mark run as Success ---------------------------------------------
        let guard = match self.repos.0.lock() {
            Ok(g) => g,
            Err(e) => {
                tracing::error!("Planner final lock: {e}");
                return PlannerProcessResult;
            }
        };
        let mut final_run = match guard.runs.get_by_id(&run.id) {
            Ok(Some(r)) => r,
            _ => return PlannerProcessResult,
        };
        final_run.status = RunStatus::Success.as_str().to_string();
        final_run.ended_at = Some(now_iso.clone());
        final_run.updated_at = now_iso.clone();
        if let Err(e) = guard.runs.upsert(&final_run) {
            tracing::error!("Planner complete run: {e}");
        }
        // Release the queue item slot so the next item can be claimed.
        if let Ok(Some(mut qi)) = guard.queue.get_by_id(&item.id) {
            qi.status = "completed".into();
            qi.finished_at = Some(now_iso.clone());
            qi.updated_at = now_iso.clone();
            if let Err(e) = guard.queue.upsert(&qi) {
                tracing::error!("Planner release queue item: {e}");
            }
        }

        tracing::info!("Planner pipeline complete (loop={loop_id})");
        PlannerProcessResult
    }
}
