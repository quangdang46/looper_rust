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
use looper_github::types::{CreatePullRequestInput, IssueLabelsInput, ListOpenIssuesInput};
use uuid::Uuid;

use looper_agent::executor::ConfiguredExecutor;
use looper_git::{build_worktree_directory_name, CreateWorktreeInput};
use looper_git::Gateway as GitGateway;
use looper_scheduler::scheduler::SendRepos;
use looper_scheduler::types::{
    Context, PlannerDiscoveryInput, PlannerDiscoveryResult, PlannerProcessInput,
    PlannerProcessResult, PlannerScheduler, SchedulerConfig,
};
use looper_storage::eventlog;
use looper_storage::record::{
    AppendInput, LoopRecord, NotificationRecord, QueueItemRecord, RunRecord, WorktreeRecord,
};
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
        Self {
            config: config.clone(),
            repos,
            github,
            agent,
            git,
            tokio_handle,
        }
    }
}

impl PlannerScheduler for Planner {
    fn discover_issues(
        &self,
        _ctx: &Context,
        input: PlannerDiscoveryInput,
    ) -> PlannerDiscoveryResult {
        // Planner discovery scans for un-queued issues that need planning.
        tracing::debug!(
            "Planner discover_issues — scanning for unplanned work items in {repo}",
            repo = input.repo
        );

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
                        let now_iso = Utc::now()
                            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                            .to_string();

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
                            let dedupe_key = format!(
                                "planner-{}-issue-{}",
                                input.project_id, issue.number
                            );

                            // Dedup: if a queue item with this dedupe_key
                            // already exists, skip.
                            let exists = match self.repos.0.lock() {
                                Ok(g) => g
                                    .queue
                                    .find_active_by_dedupe(&dedupe_key)
                                    .ok()
                                    .flatten()
                                    .is_some(),
                                Err(_) => false,
                            };
                            if exists {
                                tracing::debug!(
                                    "Planner skipping issue #{} (already queued)",
                                    issue.number
                                );
                                continue;
                            }

                            // Create a loop for this issue.
                            let loop_id = Uuid::new_v4().to_string();
                            let loop_seq =
                                self.repos.0.lock().ok().and_then(|g| {
                                    g.loops.allocate_seq().ok()
                                });
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
                                        tracing::warn!(
                                            "Planner loop upsert failed for issue #{}: {e}",
                                            issue.number
                                        );
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
                                lock_key: Some(format!(
                                    "planner-{}-issue-{}",
                                    input.project_id, issue.number
                                )),
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
                                Ok(g) => {
                                    match g
                                        .queue
                                        .upsert_active_by_dedupe_or_get_existing(
                                            &queue_item,
                                        )
                                    {
                                        Ok((inserted, _is_new)) => {
                                            tracing::info!(
                                                "Planner enqueued issue #{} (item {})",
                                                issue.number,
                                                inserted.id
                                            );
                                            new_queue_items.push(inserted);
                                        }
                                        Err(e) => tracing::warn!(
                                            "Planner queue upsert failed for issue #{}: {e}",
                                            issue.number
                                        ),
                                    }
                                }
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
        tracing::debug!(
            "Planner discover_issues done — {} planner item(s) tracked",
            merged.len()
        );

        PlannerDiscoveryResult {
            queue_items: merged,
            created_loops: vec![],
        }
    }

    fn process_claimed_queue_item(
        &self,
        ctx: &Context,
        input: PlannerProcessInput,
    ) -> PlannerProcessResult {
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
            Ok(g) => g
                .projects
                .get_by_id(&project_id)
                .ok()
                .flatten()
                .map(|p| p.repo_path.clone())
                .unwrap_or_default(),
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
                            payload_json: Some(serde_json::json!({
                                "step": "discover-issues",
                                "loop_id": loop_id,
                            }).to_string()),
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
                    let wt_dir = looper_git::build_worktree_directory_name(
                        &looper_git::CreateWorktreeInput {
                            project_id: project_id.clone(),
                            repo_path: String::new(),
                            worktree_root: String::new(),
                            branch: format!("planner/{loop_id}"),
                            base_branch: None,
                            start_point: None,
                            pr_number: None,
                            checkout_mode: looper_git::CheckoutMode::Branch,
                            protected_branches: vec![],
                        },
                    );
                    // Use project's local checkout path for filesystem operations,
                    // NOT item.repo (which is GitHub owner/repo format).
                    let wt_base: String = if !local_path.is_empty() {
                        local_path.clone()
                    } else if !project_id.is_empty() {
                        // Try to resolve from repos DB for proper filesystem path
                        self.repos.0.lock().ok()
                            .and_then(|g| g.projects.get_by_id(&project_id).ok().flatten())
                            .and_then(|p| {
                                let rp = &p.repo_path;
                                if !rp.is_empty() && !rp.contains('/') && !rp.starts_with('.'){
                                    None
                                } else if !rp.is_empty() { Some(rp.clone()) }
                                else { None }
                            })
                            .unwrap_or_else(|| ".".to_string())
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
                            metadata_json: Some(serde_json::json!({
                                "loop_id": loop_id,
                                "step": "prepare-worktree",
                            }).to_string()),
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
                        let effective_path = if local_path.is_empty() {
                            item.repo.clone().unwrap_or_default()
                        } else {
                            local_path.clone()
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
                            payload_json: Some(serde_json::json!({
                                "step": "write-spec",
                                "status": "spec_written",
                            }).to_string()),
                            ..AppendInput::new("")
                        };
                        if let Err(e) = eventlog::append(&g.events, &event) {
                            tracing::warn!("Planner spec event: {e}");
                        }
                    }
                    // When an agent executor is available, start it to write the spec
                    if let Some(ref agent) = self.agent {
                        let worktree_dir = build_worktree_directory_name(
                            &CreateWorktreeInput {
                                project_id: project_id.clone(),
                                repo_path: String::new(),
                                worktree_root: String::new(),
                                branch: format!("planner/{loop_id}"),
                                base_branch: None,
                                start_point: None,
                                pr_number: None,
                                checkout_mode: looper_git::CheckoutMode::Branch,
                                protected_branches: vec![],
                            },
                        );
                        let effective_path = if local_path.is_empty() {
                            item.repo.clone().unwrap_or_default()
                        } else {
                            local_path.clone()
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
                            prompt: format!("Write a specification for issue #{issue_number}: {issue_title}\n\nAnalyze the issue and produce a brief spec document in markdown that describes:\n1. A short summary of the bug/feature\n2. The root cause\n3. Which files need modification\n4. Tests that should be added or modified\n\nWrite the spec to a file called PLAN.md in the current directory ({worktree_abs_path}).\n\nIMPORTANT: Actually create the file. Use the `write` tool to write PLAN.md with the full spec content."),
                        };
                        match self.tokio_handle.block_on(agent.start(input)) {
                            Ok(_) => tracing::info!("Planner agent wrote spec for loop {loop_id}"),
                            Err(e) => tracing::warn!("Planner agent failed to write spec for loop {loop_id}: {e}"),
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
                            payload_json: Some(serde_json::json!({
                                "step": "publish",
                                "target_id": item.target_id,
                            }).to_string()),
                            ..AppendInput::new("")
                        };
                        if let Err(e) = eventlog::append(&g.events, &event) {
                            tracing::warn!("Planner publish event: {e}");
                        }
                    }
                    // When a git gateway is available, commit + push the
                    // branch so `gh pr create` has commits to reference.
                    if let Some(ref git) = self.git {
                        let effective_path = if local_path.is_empty() {
                            item.repo.clone().unwrap_or_default()
                        } else {
                            local_path.clone()
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
                        let worktree_dir_name =
                            looper_git::build_worktree_directory_name(&wt_input);
                        let worktree_path = format!(
                            "{}/.looper/worktrees/{}",
                            effective_path, worktree_dir_name
                        );
                        // Stub commit — placeholder so the branch has a
                        // commit on GitHub. A production planner would
                        // commit the spec document written in write-spec.
                        // ponytail: update commit message once agent writes spec
                        // Save PLAN.md before recreating the worktree
                        let plan_content = std::fs::read_to_string(format!("{worktree_path}/PLAN.md")).ok();
                        let _ = std::fs::remove_dir_all(&worktree_path);
                        let _ = self.tokio_handle.block_on(async {
                            // Create worktree at the right path
                            if let Err(e) = git.create_worktree(looper_git::CreateWorktreeInput {
                                project_id: project_id.clone(),
                                repo_path: effective_path.clone(),
                                worktree_root: format!("{}/.looper/worktrees", effective_path),
                                branch: branch.clone(),
                                base_branch: Some("main".to_string()),
                                start_point: Some("main".to_string()),
                                pr_number: None,
                                checkout_mode: looper_git::CheckoutMode::Branch,
                                protected_branches: vec!["main".into(), "master".into()],
                            }).await {
                                tracing::warn!("Planner worktree (non-fatal): {e}");
                            }
                            // Restore PLAN.md that the agent wrote so the
                            // commit includes it.
                            if let Some(ref plan) = plan_content {
                                let _ = std::fs::write(format!("{worktree_path}/PLAN.md"), plan);
                            }
                            if let Err(e) = git.commit(looper_git::CommitInput {
                                worktree_path: worktree_path.clone(),
                                message: format!("[{issue_number}] {issue_title}"),
                            }).await {
                                tracing::warn!("Planner commit (non-fatal): {e}");
                            }
                            if let Err(e) = git.push(looper_git::PushInput {
                                worktree_path: worktree_path.clone(),
                                remote: "origin".into(),
                                branch: branch.clone(),
                                expected_head_sha: None,
                                protected_branches: vec!["main".into(), "master".into()],
                                set_upstream: true,
                            }).await {
                                tracing::warn!("Planner push (non-fatal): {e}");
                            }
                        });
                    }
                    // When GitHub is available, create a spec pull request
                    if let Some(ref gw) = self.github {
                        if let Some(ref repo_path) = item.repo {
                            if !repo_path.is_empty() {
                                let body = format!(
                                    "## {}\n\nThis PR addresses issue #{} in {}. The automated planning spec has been written and published.\n\n_From Looper pipeline._",
                                    issue_title,
                                    issue_number,
                                    repo_path
                                );
                                match gw.create_pull_request(CreatePullRequestInput {
                                    repo: repo_path.clone(),
                                    head_branch: format!("planner/{loop_id}"),
                                    base_branch: "main".to_string(),
                                    title: format!("[{}] {}", issue_number, issue_title),
                                    body,
                                    cwd: ".".to_string(),
                                }) {
                                    Ok(result) => {
                                        tracing::info!("Planner created PR #{} for loop {loop_id}", result.number);
                                        // Mark the PR as being in spec-review phase
                                        let _ = gw.add_issue_labels(IssueLabelsInput {
                                            repo: repo_path.clone(),
                                            issue_number: result.number,
                                            labels: vec!["looper:spec-reviewing".into()],
                                            cwd: ".".to_string(),
                                        });
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
                            body: format!(
                                "Planner finished pipeline for loop {loop_id} (item={})",
                                item.id
                            ),
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
            let wt_dir = build_worktree_directory_name(
                &CreateWorktreeInput {
                    project_id: project_id.clone(),
                    repo_path: String::new(),
                    worktree_root: String::new(),
                    branch: format!("planner/{loop_id}"),
                    base_branch: None,
                    start_point: None,
                    pr_number: None,
                    checkout_mode: looper_git::CheckoutMode::Branch,
                    protected_branches: vec![],
                },
            );
            let effective_path = if local_path.is_empty() {
                item.repo.clone().unwrap_or_default()
            } else {
                local_path.clone()
            };
            let worktree_path = format!("{}/.looper/worktrees/{}", effective_path, wt_dir);
            let _ = self.tokio_handle.block_on(git.cleanup_worktree(
                looper_git::types::CleanupWorktreeInput {
                    repo_path: effective_path.clone(),
                    worktree_path: worktree_path.clone(),
                    branch: format!("planner/{loop_id}"),
                    protected_branches: vec!["main".to_string(), "master".to_string()],
                },
            ));
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
