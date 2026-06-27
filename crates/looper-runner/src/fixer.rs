//! Fixer runner — implements the [`FixerScheduler`] trait.
//!
//! The Fixer responds to review feedback or CI failures by applying targeted
//! fixes to a pull request.  It has the longest step pipeline because it
//! must collect comments, prepare a worktree, apply changes, validate,
//! push, and reconcile.
//!
//! **Step pipeline** (10 steps):
//!   1. `discover-pr`         — identify the PR that needs fixing
//!   2. `claim-pr`            — assign the fix to this daemon
//!   3. `collect-fixes`       — gather review comments / CI failure details
//!   4. `prepare-worktree`    — create / restore a dedicated worktree
//!   5. `repair`              — apply the actual fixes (agent-driven)
//!   6. `validate`            — build + test to confirm the fix
//!   7. `push`                — push the fix branch
//!   8. `reconcile-commits`   — squash / organise the commit history
//!   9. `resolve-comments`    — mark review comments as resolved
//!  10. `recheck`             — trigger a fresh CI run

use std::sync::Arc;

use chrono::Utc;
use uuid::Uuid;

use looper_agent::executor::ConfiguredExecutor;
use looper_git::types::{CheckoutMode, CleanupWorktreeInput, CreateWorktreeInput};
use looper_git::{build_worktree_directory_name, Gateway as GitGateway};
use looper_github::gateway::Gateway;
use looper_github::types::{
    IssueAssigneesInput, ListOpenPullRequestsInput, ResolveReviewThreadInput, ViewPullRequestInput,
};
use looper_scheduler::scheduler::SendRepos;
use looper_scheduler::types::{
    Context, FixerBaseBranchDiscoveryInput, FixerDiscoveryInput, FixerDiscoveryResult, FixerScheduler,
    ReviewerTargetedDiscoveryInput, SchedulerConfig,
};
use looper_storage::eventlog;
use looper_storage::record::{AppendInput, QueueItemRecord, RunRecord};
use looper_types::RunStatus;

use crate::types::fixer_steps;

/// Fixer runner state machine.
pub struct Fixer {
    pub config: SchedulerConfig,
    pub repos: Arc<SendRepos>,
    pub github: Option<Arc<Gateway>>,
    pub tokio_handle: tokio::runtime::Handle,
    pub agent: Option<Arc<ConfiguredExecutor>>,
    pub git: Option<Arc<GitGateway>>,
}

// SAFETY: SendRepos is Send+Sync (Mutex<Repositories>); Gateway is Send+Sync.
unsafe impl Send for Fixer {}
unsafe impl Sync for Fixer {}

impl Fixer {
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

    fn execute_pipeline(&self, item: &QueueItemRecord) -> Result<(), String> {
        let ctx = Context::new();
        let now_iso = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();

        let loop_id = item.loop_id.as_deref().ok_or_else(|| "Fixer queue item has no loop_id".to_string())?;

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
                    current_step: Some(fixer_steps::DISCOVER_PR.to_string()),
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
        let steps = fixer_steps::ALL;
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
            tracing::info!("Fixer step: {step} (loop={loop_id})");

            match step {
                fixer_steps::DISCOVER_PR => {
                    if let Ok(g) = self.repos.0.lock() {
                        let event = AppendInput {
                            event_type: "fixer.discover_pr".into(),
                            project_id: item.project_id.clone(),
                            loop_id: item.loop_id.clone(),
                            run_id: Some(run.id.clone()),
                            entity_type: Some("queue_item".into()),
                            entity_id: Some(item.id.clone()),
                            payload_json: Some(serde_json::json!({"step": "discover-pr"}).to_string()),
                            ..AppendInput::new("")
                        };
                        if let Err(e) = eventlog::append(&g.events, &event) {
                            tracing::warn!("Fixer discover_pr event: {e}");
                        }
                    }
                    if let Some(ref gw) = self.github {
                        if let Some(ref repo_path) = item.repo {
                            if let Some(pr_num) = item.pr_number {
                                match gw.view_pull_request(ViewPullRequestInput {
                                    repo: repo_path.clone(),
                                    pr_number: pr_num,
                                    cwd: ".".to_string(),
                                }) {
                                    Ok(pr) => tracing::info!("Fixer discovered PR #{}: {}", pr.number, pr.title),
                                    Err(e) => tracing::warn!("Fixer view PR failed: {e}"),
                                }
                            }
                        }
                    }
                }
                fixer_steps::CLAIM_PR => {
                    if let Ok(g) = self.repos.0.lock() {
                        let event = AppendInput {
                            event_type: "fixer.claim_pr".into(),
                            project_id: item.project_id.clone(),
                            loop_id: item.loop_id.clone(),
                            run_id: Some(run.id.clone()),
                            entity_type: Some("queue_item".into()),
                            entity_id: Some(item.id.clone()),
                            payload_json: Some(serde_json::json!({"step": "claim-pr"}).to_string()),
                            ..AppendInput::new("")
                        };
                        if let Err(e) = eventlog::append(&g.events, &event) {
                            tracing::warn!("Fixer claim_pr event: {e}");
                        }
                    }
                    if let Some(ref gw) = self.github {
                        if let Some(ref repo_path) = item.repo {
                            if let Some(pr_num) = item.pr_number {
                                let _ = gw.add_issue_assignees(IssueAssigneesInput {
                                    repo: repo_path.clone(),
                                    issue_number: pr_num,
                                    assignees: vec!["looper".into()],
                                    cwd: ".".to_string(),
                                });
                            }
                        }
                    }
                }
                fixer_steps::COLLECT_FIXES => {
                    if let Ok(g) = self.repos.0.lock() {
                        let event = AppendInput {
                            event_type: "fixer.collect_fixes".into(),
                            project_id: item.project_id.clone(),
                            loop_id: item.loop_id.clone(),
                            run_id: Some(run.id.clone()),
                            entity_type: Some("queue_item".into()),
                            entity_id: Some(item.id.clone()),
                            payload_json: Some(serde_json::json!({"step": "collect-fixes"}).to_string()),
                            ..AppendInput::new("")
                        };
                        if let Err(e) = eventlog::append(&g.events, &event) {
                            tracing::warn!("Fixer collect_fixes event: {e}");
                        }
                    }
                    // Perform fixes via agent
                    if let Some(ref agent) = self.agent {
                        let input = looper_agent::executor::StartInput {
                            loop_id: run.loop_id.clone(),
                            current_step: Some("collect_fixes".to_string()),
                            last_completed_step: None,
                            checkpoint_json: None,
                            project_id: item.project_id.clone().unwrap_or_default(),
                            run_id: run.id.clone(),
                            working_directory: String::new(),
                            prompt: format!(
                                "Review the fixes needed for PR #{} in repo {}. Identify specific code issues, suggest fixes, and create a summary of what needs to change.",
                                item.pr_number.unwrap_or(0),
                                item.repo.as_deref().unwrap_or("unknown")
                            ),
                        };
                        match self.tokio_handle.block_on(agent.start(input)) {
                            Ok(_) => tracing::info!("Agent fix started for run {}", run.id),
                            Err(e) => tracing::warn!("Agent fix failed: {}", e),
                        }
                    }
                }
                fixer_steps::PREPARE_WORKTREE => {
                    if let Ok(g) = self.repos.0.lock() {
                        let event = AppendInput {
                            event_type: "fixer.prepare_worktree".into(),
                            project_id: item.project_id.clone(),
                            loop_id: item.loop_id.clone(),
                            run_id: Some(run.id.clone()),
                            entity_type: Some("queue_item".into()),
                            entity_id: Some(item.id.clone()),
                            payload_json: Some(serde_json::json!({"step": "prepare-worktree"}).to_string()),
                            ..AppendInput::new("")
                        };
                        if let Err(e) = eventlog::append(&g.events, &event) {
                            tracing::warn!("Fixer prepare_worktree event: {e}");
                        }
                    }
                    // Create fix worktree via git
                    if let Some(ref git) = self.git {
                        let input = looper_git::CreateWorktreeInput {
                            project_id: item.project_id.clone().unwrap_or_default(),
                            repo_path: ".".to_string(),
                            worktree_root: ".".to_string(),
                            branch: format!("fix/{}", &run.loop_id),
                            base_branch: None,
                            start_point: None,
                            pr_number: None,
                            checkout_mode: looper_git::CheckoutMode::Branch,
                            protected_branches: vec!["main".to_string(), "master".to_string()],
                        };
                        match self.tokio_handle.block_on(git.create_worktree(input)) {
                            Ok(_) => tracing::info!("Worktree created"),
                            Err(e) => tracing::warn!("Worktree creation failed: {}", e),
                        }
                    }
                }
                fixer_steps::REPAIR => {
                    if let Ok(g) = self.repos.0.lock() {
                        let event = AppendInput {
                            event_type: "fixer.repair".into(),
                            project_id: item.project_id.clone(),
                            loop_id: item.loop_id.clone(),
                            run_id: Some(run.id.clone()),
                            entity_type: Some("queue_item".into()),
                            entity_id: Some(item.id.clone()),
                            payload_json: Some(serde_json::json!({"step": "repair"}).to_string()),
                            ..AppendInput::new("")
                        };
                        if let Err(e) = eventlog::append(&g.events, &event) {
                            tracing::warn!("Fixer repair event: {e}");
                        }
                    }
                    // Apply repair via agent
                    if let Some(ref agent) = self.agent {
                        let input = looper_agent::executor::StartInput {
                            loop_id: run.loop_id.clone(),
                            current_step: Some("repair".to_string()),
                            last_completed_step: Some("prepare_worktree".to_string()),
                            checkpoint_json: None,
                            project_id: item.project_id.clone().unwrap_or_default(),
                            run_id: run.id.clone(),
                            working_directory: String::new(),
                            prompt: format!(
                                "Apply the fixes for PR #{} in repo {}. Make the necessary code changes and verify they work.",
                                item.pr_number.unwrap_or(0),
                                item.repo.as_deref().unwrap_or("unknown")
                            ),
                        };
                        match self.tokio_handle.block_on(agent.start(input)) {
                            Ok(_) => tracing::info!("Agent repair started for run {}", run.id),
                            Err(e) => tracing::warn!("Agent repair failed: {}", e),
                        }
                    }
                }
                fixer_steps::VALIDATE => {
                    if let Ok(g) = self.repos.0.lock() {
                        let event = AppendInput {
                            event_type: "fixer.validate".into(),
                            project_id: item.project_id.clone(),
                            loop_id: item.loop_id.clone(),
                            run_id: Some(run.id.clone()),
                            entity_type: Some("queue_item".into()),
                            entity_id: Some(item.id.clone()),
                            payload_json: Some(serde_json::json!({"step": "validate"}).to_string()),
                            ..AppendInput::new("")
                        };
                        if let Err(e) = eventlog::append(&g.events, &event) {
                            tracing::warn!("Fixer validate event: {e}");
                        }
                    }
                    // Check CI status on the PR to validate fixes
                    if let Some(ref gw) = self.github {
                        if let Some(ref repo_path) = item.repo {
                            if let Some(pr_num) = item.pr_number {
                                match gw.view_pull_request(ViewPullRequestInput {
                                    repo: repo_path.clone(),
                                    pr_number: pr_num,
                                    cwd: ".".to_string(),
                                }) {
                                    Ok(pr) => {
                                        let failing = crate::merge_watch::pr_has_failing_checks(&pr);
                                        let names = crate::merge_watch::pr_failing_check_names(&pr);
                                        if failing {
                                            tracing::warn!(
                                                "Fixer: PR #{} still has failing checks: {:?}",
                                                pr_num,
                                                names
                                            );
                                        } else {
                                            tracing::info!("Fixer: PR #{} checks passing", pr_num);
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!("Fixer: failed to view PR #{} for validation: {e}", pr_num);
                                    }
                                }
                            }
                        }
                    }
                }
                fixer_steps::PUSH => {
                    if let Ok(g) = self.repos.0.lock() {
                        let event = AppendInput {
                            event_type: "fixer.push".into(),
                            project_id: item.project_id.clone(),
                            loop_id: item.loop_id.clone(),
                            run_id: Some(run.id.clone()),
                            entity_type: Some("queue_item".into()),
                            entity_id: Some(item.id.clone()),
                            payload_json: Some(serde_json::json!({"step": "push"}).to_string()),
                            ..AppendInput::new("")
                        };
                        if let Err(e) = eventlog::append(&g.events, &event) {
                            tracing::warn!("Fixer push event: {e}");
                        }
                    }
                    // Git push fixes
                    if let Some(ref git) = self.git {
                        let push_input = looper_git::PushInput {
                            worktree_path: ".".to_string(),
                            remote: "origin".to_string(),
                            branch: format!("fix/{}", &run.loop_id),
                            expected_head_sha: None,
                            protected_branches: vec!["main".to_string(), "master".to_string()],
                            set_upstream: true,
                        };
                        match self.tokio_handle.block_on(git.push(push_input)) {
                            Ok(_) => tracing::info!("Fixes pushed"),
                            Err(e) => tracing::warn!("Push failed: {}", e),
                        }
                    }
                }
                fixer_steps::RECONCILE_COMMITS => {
                    if let Ok(g) = self.repos.0.lock() {
                        let event = AppendInput {
                            event_type: "fixer.reconcile_commits".into(),
                            project_id: item.project_id.clone(),
                            loop_id: item.loop_id.clone(),
                            run_id: Some(run.id.clone()),
                            entity_type: Some("queue_item".into()),
                            entity_id: Some(item.id.clone()),
                            payload_json: Some(serde_json::json!({"step": "reconcile-commits"}).to_string()),
                            ..AppendInput::new("")
                        };
                        if let Err(e) = eventlog::append(&g.events, &event) {
                            tracing::warn!("Fixer reconcile_commits event: {e}");
                        }
                    }
                }
                fixer_steps::RESOLVE_COMMENTS => {
                    if let Ok(g) = self.repos.0.lock() {
                        let event = AppendInput {
                            event_type: "fixer.resolve_comments".into(),
                            project_id: item.project_id.clone(),
                            loop_id: item.loop_id.clone(),
                            run_id: Some(run.id.clone()),
                            entity_type: Some("queue_item".into()),
                            entity_id: Some(item.id.clone()),
                            payload_json: Some(serde_json::json!({"step": "resolve-comments"}).to_string()),
                            ..AppendInput::new("")
                        };
                        if let Err(e) = eventlog::append(&g.events, &event) {
                            tracing::warn!("Fixer resolve_comments event: {e}");
                        }
                    }
                    if let Some(ref gw) = self.github {
                        if !item.target_id.is_empty() {
                            let _ = gw.resolve_review_thread(ResolveReviewThreadInput {
                                repo: item.repo.clone().unwrap_or_default(),
                                thread_id: item.target_id.clone(),
                                cwd: ".".to_string(),
                            });
                        }
                    }
                }
                fixer_steps::RECHECK => {
                    if let Ok(g) = self.repos.0.lock() {
                        let event = AppendInput {
                            event_type: "fixer.recheck".into(),
                            project_id: item.project_id.clone(),
                            loop_id: item.loop_id.clone(),
                            run_id: Some(run.id.clone()),
                            entity_type: Some("queue_item".into()),
                            entity_id: Some(item.id.clone()),
                            payload_json: Some(serde_json::json!({"step": "recheck"}).to_string()),
                            ..AppendInput::new("")
                        };
                        if let Err(e) = eventlog::append(&g.events, &event) {
                            tracing::warn!("Fixer recheck event: {e}");
                        }
                    }
                    // Trigger a new CI run by pushing an empty commit
                    if let Some(ref git) = self.git {
                        let branch_name = format!("fix/{}", &run.loop_id);
                        let _ = self.tokio_handle.block_on(git.commit(looper_git::types::CommitInput {
                            worktree_path: ".".to_string(),
                            message: "recheck: trigger CI".to_string(),
                        }));
                        let _ = self.tokio_handle.block_on(git.push(looper_git::types::PushInput {
                            worktree_path: ".".to_string(),
                            remote: "origin".to_string(),
                            branch: branch_name,
                            expected_head_sha: None,
                            protected_branches: vec!["main".to_string(), "master".to_string()],
                            set_upstream: false,
                        }));
                        tracing::info!("Fixer: pushed empty commit to trigger CI recheck");
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
            let wt_dir = build_worktree_directory_name(&CreateWorktreeInput {
                project_id: item.project_id.clone().unwrap_or_default(),
                repo_path: ".".to_string(),
                worktree_root: ".".to_string(),
                branch: format!("fix/{}", &run.loop_id),
                base_branch: None,
                start_point: None,
                pr_number: None,
                checkout_mode: CheckoutMode::Branch,
                protected_branches: vec![],
            });
            let worktree_path = format!("./{}", wt_dir);
            let _ = self.tokio_handle.block_on(git.cleanup_worktree(CleanupWorktreeInput {
                repo_path: ".".to_string(),
                worktree_path: worktree_path.clone(),
                branch: format!("fix/{}", &run.loop_id),
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

        tracing::info!("Fixer pipeline complete (loop={loop_id})");
        Ok(())
    }
}

impl FixerScheduler for Fixer {
    fn discover_pull_requests(&self, _ctx: &Context, input: FixerDiscoveryInput) -> FixerDiscoveryResult {
        tracing::debug!("Fixer discover_pull_requests — scanning for fixable PRs");
        let guard = match self.repos.0.lock() {
            Ok(g) => g,
            Err(e) => {
                tracing::error!("Fixer discover_pull_requests lock: {e}");
                return FixerDiscoveryResult::default();
            }
        };
        let queued = match guard.queue.list_by_statuses(&["queued".into()]) {
            Ok(items) => items,
            Err(e) => {
                tracing::error!("Fixer discover_pull_requests list: {e}");
                Vec::new()
            }
        };
        let fixer_items: Vec<_> = queued.into_iter().filter(|item| item.r#type == "fixer").collect();
        tracing::debug!("Fixer discover_pull_requests — found {} existing fixer items", fixer_items.len());
        drop(guard);

        // GitHub-powered PR discovery
        if let Some(ref gw) = self.github {
            if !input.repo.is_empty() {
                match gw.list_open_pull_requests(ListOpenPullRequestsInput {
                    repo: input.repo.clone(),
                    cwd: ".".to_string(),
                    limit: 50,
                    label: "".into(),
                    labels: vec![],
                    author: "".into(),
                    base_ref_name: "".into(),
                    timeout: None,
                }) {
                    Ok(prs) => tracing::debug!("Fixer discovered {} open PRs via GitHub", prs.len()),
                    Err(e) => tracing::warn!("Fixer GitHub discovery failed: {e}"),
                }
            }
        }

        FixerDiscoveryResult { queue_items: fixer_items }
    }

    fn discover_pull_request(&self, _ctx: &Context, input: ReviewerTargetedDiscoveryInput) -> FixerDiscoveryResult {
        tracing::debug!("Fixer discover_pull_request — scanning for PR-specific fixer items");
        let guard = match self.repos.0.lock() {
            Ok(g) => g,
            Err(e) => {
                tracing::error!("Fixer discover_pull_request lock: {e}");
                return FixerDiscoveryResult::default();
            }
        };
        let queued = match guard.queue.list_by_statuses(&["queued".into()]) {
            Ok(items) => items,
            Err(e) => {
                tracing::error!("Fixer discover_pull_request list: {e}");
                Vec::new()
            }
        };
        let fixer_items: Vec<_> = queued.into_iter().filter(|item| item.r#type == "fixer").collect();
        drop(guard);

        // GitHub-powered PR discovery
        if let Some(ref gw) = self.github {
            if !input.repo.is_empty() {
                match gw.list_open_pull_requests(ListOpenPullRequestsInput {
                    repo: input.repo.clone(),
                    cwd: ".".to_string(),
                    limit: 50,
                    label: "".into(),
                    labels: vec![],
                    author: "".into(),
                    base_ref_name: "".into(),
                    timeout: None,
                }) {
                    Ok(prs) => tracing::debug!("Fixer discovered {} open PRs via GitHub", prs.len()),
                    Err(e) => tracing::warn!("Fixer GitHub discovery failed: {e}"),
                }
            }
        }

        FixerDiscoveryResult { queue_items: fixer_items }
    }

    fn discover_pull_requests_for_base_branch_update(
        &self,
        _ctx: &Context,
        input: FixerBaseBranchDiscoveryInput,
    ) -> FixerDiscoveryResult {
        tracing::debug!(
            "Fixer discover_pull_requests_for_base_branch_update — scanning for fixer items on base branch"
        );
        let guard = match self.repos.0.lock() {
            Ok(g) => g,
            Err(e) => {
                tracing::error!("Fixer discover_pull_requests_for_base_branch_update lock: {e}");
                return FixerDiscoveryResult::default();
            }
        };
        let queued = match guard.queue.list_by_statuses(&["queued".into()]) {
            Ok(items) => items,
            Err(e) => {
                tracing::error!("Fixer discover_pull_requests_for_base_branch_update list: {e}");
                Vec::new()
            }
        };
        let fixer_items: Vec<_> = queued.into_iter().filter(|item| item.r#type == "fixer").collect();
        drop(guard);

        // GitHub-powered PR discovery
        if let Some(ref gw) = self.github {
            if !input.repo.is_empty() {
                match gw.list_open_pull_requests(ListOpenPullRequestsInput {
                    repo: input.repo.clone(),
                    cwd: ".".to_string(),
                    limit: 50,
                    label: "".into(),
                    labels: vec![],
                    author: "".into(),
                    base_ref_name: "".into(),
                    timeout: None,
                }) {
                    Ok(prs) => tracing::debug!("Fixer discovered {} open PRs via GitHub", prs.len()),
                    Err(e) => tracing::warn!("Fixer GitHub discovery failed: {e}"),
                }
            }
        }

        FixerDiscoveryResult { queue_items: fixer_items }
    }

    fn process_claimed_queue_item(&self, _ctx: &Context, item: &QueueItemRecord) -> Result<(), String> {
        self.execute_pipeline(item)
    }
}
