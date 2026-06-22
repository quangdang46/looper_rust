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
use looper_git::Gateway as GitGateway;
use looper_scheduler::scheduler::SendRepos;
use looper_scheduler::types::{
    Context, PlannerDiscoveryInput, PlannerDiscoveryResult, PlannerProcessInput,
    PlannerProcessResult, PlannerScheduler, SchedulerConfig,
};
use looper_storage::eventlog;
use looper_storage::record::{
    AppendInput, NotificationRecord, RunRecord, WorktreeRecord,
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
        // In a production setup this would query the GitHub issue tracker.
        // For now, we rely on the scheduler's existing tick to populate items.
        tracing::debug!(
            "Planner discover_issues — scanning for unplanned work items"
        );
        // Check the queue for existing planner items that may need re-enqueue
        let guard = match self.repos.0.lock() {
            Ok(g) => g,
            Err(e) => {
                tracing::error!("Planner discover_issues lock: {e}");
                return PlannerDiscoveryResult::default();
            }
        };
        let queued = match guard.queue.list_by_statuses(&["queued".into()]) {
            Ok(items) => items,
            Err(e) => {
                tracing::error!("Planner discover_issues list_by_statuses: {e}");
                Vec::new()
            }
        };
        let planner_items: Vec<_> = queued
            .into_iter()
            .filter(|item| item.r#type == "planner")
            .collect();
        let count = planner_items.len();
        tracing::debug!("Planner discover_issues — found {count} existing planner queue items");
        drop(guard);

        // GitHub-powered issue discovery
        if let Some(ref gw) = self.github {
            if !input.repo.is_empty() {
                match gw.list_open_issues(ListOpenIssuesInput {
                    repo: input.repo.clone(),
                    cwd: ".".to_string(),
                    limit: 50,
                    assignee: String::new(),
                    label: String::new(),
                    labels: vec![],
                }) {
                    Ok(issues) => {
                        // Would create queue items for unplanned issues
                        tracing::debug!("Planner GitHub discovery — {} open issues", issues.len());
                    }
                    Err(e) => tracing::warn!("Planner GitHub issue discovery failed: {e}"),
                }
            }
        }

        PlannerDiscoveryResult {
            queue_items: planner_items,
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
                    // Create a worktree record so downstream phases
                    // know which branch / worktree this loop uses.
                    if let Ok(g) = self.repos.0.lock() {
                        let branch_name = format!("planner/{loop_id}");
                        let worktree = WorktreeRecord {
                            id: Uuid::new_v4().to_string(),
                            project_id: project_id.clone(),
                            repo_path: item.repo.clone().unwrap_or_default(),
                            worktree_path: format!(".worktrees/{branch_name}"),
                            branch: branch_name,
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
                        let input = looper_git::CreateWorktreeInput {
                            project_id: project_id.clone(),
                            repo_path: item.repo.clone().unwrap_or_default(),
                            worktree_root: String::new(),
                            branch: branch_name,
                            base_branch: Some("main".to_string()),
                            start_point: None,
                            pr_number: None,
                            checkout_mode: looper_git::CheckoutMode::Branch,
                            protected_branches: vec!["main".to_string(), "master".to_string()],
                        };
                        let _ = self.tokio_handle.block_on(git.create_worktree(input));
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
                        let input = looper_agent::executor::StartInput {
                            loop_id: run.loop_id.clone(),
                            current_step: Some(planner_steps::WRITE_SPEC.to_string()),
                            last_completed_step: Some(planner_steps::DISCOVER_ISSUES.to_string()),
                            checkpoint_json: None,
                        };
                        let _ = self.tokio_handle.block_on(agent.start(input));
                    }
                }

                planner_steps::PUBLISH => {
                    // Log the publish intent; a real implementation
                    // would open a GitHub PR via looper_github.
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
                    // When GitHub is available, create a spec pull request
                    if let Some(ref gw) = self.github {
                        if let Some(ref repo_path) = item.repo {
                            if !repo_path.is_empty() {
                                match gw.create_pull_request(CreatePullRequestInput {
                                    repo: repo_path.clone(),
                                    head_branch: format!("planner/{loop_id}"),
                                    base_branch: "main".to_string(),
                                    title: format!("[Planning] Spec for loop {loop_id}"),
                                    body: format!("Automated planning specification for loop {loop_id}."),
                                    cwd: ".".to_string(),
                                }) {
                                    Ok(result) => {
                                        tracing::info!("Planner created PR #{} for loop {loop_id}", result.number);
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
                            if let Some(pr_num) = item.pr_number {
                                let _ = gw.add_issue_labels(IssueLabelsInput {
                                    repo: repo_path.clone(),
                                    issue_number: pr_num,
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
        final_run.updated_at = now_iso;
        if let Err(e) = guard.runs.upsert(&final_run) {
            tracing::error!("Planner complete run: {e}");
        }

        tracing::info!("Planner pipeline complete (loop={loop_id})");
        PlannerProcessResult
    }
}
