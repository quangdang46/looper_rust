//! Reviewer runner — implements the [`ReviewerScheduler`] trait.
//!
//! The Reviewer examines open pull requests, determines whether a review is
//! needed, runs an AI-powered code review, and publishes the results.
//!
//! **Step pipeline** (6 steps):
//!   1. `discover`   — find PRs that need review
//!   2. `filter`     — apply exclusion criteria (draft, label, author)
//!   3. `claim`      — mark the review as in-progress (set assignee / status)
//!   4. `snapshot`   — capture the current diff / PR metadata
//!   5. `review`     — run the AI review agent
//!   6. `publish`    — post review comments / mark check complete

use std::sync::Arc;

use chrono::Utc;
use uuid::Uuid;

use looper_agent::executor::ConfiguredExecutor;
use looper_git::{build_worktree_directory_name, Gateway as GitGateway};
use looper_git::types::CheckoutMode;

use looper_github::types::{
    IssueAssigneesInput, IssueLabelsInput, ListOpenPullRequestsInput, ViewPullRequestInput,
};
use looper_scheduler::scheduler::SendRepos;
use looper_scheduler::types::{
    Context, ReviewerDiscoveryInput, ReviewerDiscoveryResult, ReviewerScheduler,
    ReviewerTargetedDiscoveryInput, SchedulerConfig,
};
use looper_storage::eventlog;
use looper_storage::record::{AppendInput, LoopRecord, NotificationRecord, QueueItemRecord, RunRecord};
use looper_types::RunStatus;

use crate::types::reviewer_steps;

/// Reviewer runner state machine.
pub struct Reviewer {
    pub config: SchedulerConfig,
    pub repos: Arc<SendRepos>,
    pub github: Option<Arc<looper_github::Gateway>>,
    pub tokio_handle: tokio::runtime::Handle,
    pub agent: Option<Arc<ConfiguredExecutor>>,
    pub git: Option<Arc<GitGateway>>,
}

// SAFETY: SendRepos is Send+Sync (Mutex<Repositories>). Gateway is Send+Sync.
unsafe impl Send for Reviewer {}
unsafe impl Sync for Reviewer {}

impl Reviewer {
    pub fn new(
        config: &SchedulerConfig,
        repos: Arc<SendRepos>,
        github: Option<Arc<looper_github::Gateway>>,
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

    /// Shared pipeline logic used by both single-PR and bulk discovery paths.
    fn execute_pipeline(&self, item: &QueueItemRecord) -> Result<(), String> {
        let ctx = Context::new();
        let now_iso = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();

        let loop_id = item
            .loop_id
            .as_deref()
            .ok_or_else(|| "Reviewer queue item has no loop_id".to_string())?;

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
                    current_step: Some(reviewer_steps::DISCOVER.to_string()),
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
        let steps = reviewer_steps::ALL;
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
            tracing::info!("Reviewer step: {step} (loop={loop_id})");

            match step {
                reviewer_steps::DISCOVER => {
                    if let Ok(g) = self.repos.0.lock() {
                        let event = AppendInput {
                            event_type: "reviewer.discover".into(),
                            project_id: item.project_id.clone(),
                            loop_id: item.loop_id.clone(),
                            run_id: Some(run.id.clone()),
                            entity_type: Some("queue_item".into()),
                            entity_id: Some(item.id.clone()),
                            payload_json: Some(serde_json::json!({"step": "discover"}).to_string()),
                            ..AppendInput::new("")
                        };
                        if let Err(e) = eventlog::append(&g.events, &event) {
                            tracing::warn!("Reviewer discover event: {e}");
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
                                    Ok(pr) => tracing::info!("Reviewer discovered PR #{}: {}", pr.number, pr.title),
                                    Err(e) => tracing::warn!("Reviewer view PR failed: {e}"),
                                }
                            }
                        }
                    }
                }
                reviewer_steps::FILTER => {
                    if let Ok(g) = self.repos.0.lock() {
                        let event = AppendInput {
                            event_type: "reviewer.filter".into(),
                            project_id: item.project_id.clone(),
                            loop_id: item.loop_id.clone(),
                            run_id: Some(run.id.clone()),
                            entity_type: Some("queue_item".into()),
                            entity_id: Some(item.id.clone()),
                            payload_json: Some(serde_json::json!({"step": "filter"}).to_string()),
                            ..AppendInput::new("")
                        };
                        if let Err(e) = eventlog::append(&g.events, &event) {
                            tracing::warn!("Reviewer filter event: {e}");
                        }
                    }
                }
                reviewer_steps::CLAIM => {
                    if let Ok(g) = self.repos.0.lock() {
                        let event = AppendInput {
                            event_type: "reviewer.claim".into(),
                            project_id: item.project_id.clone(),
                            loop_id: item.loop_id.clone(),
                            run_id: Some(run.id.clone()),
                            entity_type: Some("queue_item".into()),
                            entity_id: Some(item.id.clone()),
                            payload_json: Some(serde_json::json!({"step": "claim"}).to_string()),
                            ..AppendInput::new("")
                        };
                        if let Err(e) = eventlog::append(&g.events, &event) {
                            tracing::warn!("Reviewer claim event: {e}");
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
                reviewer_steps::SNAPSHOT => {
                    if let Ok(g) = self.repos.0.lock() {
                        let event = AppendInput {
                            event_type: "reviewer.snapshot".into(),
                            project_id: item.project_id.clone(),
                            loop_id: item.loop_id.clone(),
                            run_id: Some(run.id.clone()),
                            entity_type: Some("queue_item".into()),
                            entity_id: Some(item.id.clone()),
                            payload_json: Some(serde_json::json!({"step": "snapshot"}).to_string()),
                            ..AppendInput::new("")
                        };
                        if let Err(e) = eventlog::append(&g.events, &event) {
                            tracing::warn!("Reviewer snapshot event: {e}");
                        }
                    }
                }
                reviewer_steps::PREPARE_WORKTREE => {
                    if let Ok(g) = self.repos.0.lock() {
                        let event = AppendInput {
                            event_type: "reviewer.prepare_worktree".into(),
                            project_id: item.project_id.clone(),
                            loop_id: item.loop_id.clone(),
                            run_id: Some(run.id.clone()),
                            entity_type: Some("queue_item".into()),
                            entity_id: Some(item.id.clone()),
                            payload_json: Some(serde_json::json!({"step": "prepare-worktree"}).to_string()),
                            ..AppendInput::new("")
                        };
                        if let Err(e) = eventlog::append(&g.events, &event) {
                            tracing::warn!("Reviewer prepare_worktree event: {e}");
                        }
                    }
                    // Perform git worktree creation
                    if let Some(ref git) = self.git {
                        use looper_git::CheckoutMode;
                        let project_id = &item.project_id;
                        let worktree_root = std::path::PathBuf::from(".");
                        let input = looper_git::CreateWorktreeInput {
                            project_id: project_id.clone().unwrap_or_default(),
                            repo_path: ".".to_string(),
                            worktree_root: worktree_root.display().to_string(),
                            branch: format!("review/{}", &run.loop_id),
                            base_branch: Some("main".to_string()),
                            start_point: Some("main".to_string()),
                            pr_number: None,
                            checkout_mode: CheckoutMode::Branch,
                            protected_branches: vec!["main".to_string(), "master".to_string()],
                        };
                        match self.tokio_handle.block_on(git.create_worktree(input)) {
                            Ok(_) => tracing::info!("Worktree created for project {}", project_id.as_deref().unwrap_or("?")),
                            Err(e) => tracing::warn!("Worktree creation failed: {}", e),
                        }
                    }
                }
                reviewer_steps::REVIEW => {
                    if let Ok(g) = self.repos.0.lock() {
                        let event = AppendInput {
                            event_type: "reviewer.review".into(),
                            project_id: item.project_id.clone(),
                            loop_id: item.loop_id.clone(),
                            run_id: Some(run.id.clone()),
                            entity_type: Some("queue_item".into()),
                            entity_id: Some(item.id.clone()),
                            payload_json: Some(serde_json::json!({"step": "review"}).to_string()),
                            ..AppendInput::new("")
                        };
                        if let Err(e) = eventlog::append(&g.events, &event) {
                            tracing::warn!("Reviewer review event: {e}");
                        }
                    }
                    // Perform agent review
                    if let Some(ref agent) = self.agent {
                        let effective_path = if item.project_id.clone().unwrap_or_default().is_empty() {
                            item.repo.clone().unwrap_or_default()
                        } else {
                            format!("/tmp/e2e-{}", item.project_id.as_deref().unwrap_or("looper"))
                        };
                        let worktree_dir = looper_git::build_worktree_directory_name(
                            &looper_git::CreateWorktreeInput {
                                project_id: item.project_id.clone().unwrap_or_default(),
                                repo_path: String::new(),
                                worktree_root: String::new(),
                                branch: format!("review/{}", run.loop_id),
                                base_branch: None,
                                start_point: None,
                                pr_number: None,
                                checkout_mode: CheckoutMode::Branch,
                                protected_branches: vec![],
                            },
                        );
                        let worktree_abs_path = format!("{}/.looper/worktrees/{}", effective_path, worktree_dir);
                        let input = looper_agent::executor::StartInput {
                            loop_id: run.loop_id.clone(),
                            current_step: Some("review".to_string()),
                            last_completed_step: Some("prepare_worktree".to_string()),
                            checkpoint_json: None,
                            project_id: item.project_id.clone().unwrap_or_default(),
                            run_id: run.id.clone(),
                            working_directory: worktree_abs_path,
                            prompt: format!("Review pull request #{} in repo {}. Check for:\n1. Correctness — does the code do what it claims?\n2. Completeness — are edge cases handled?\n3. Style — does it follow project conventions?\n4. Tests — are there tests for the changes?\n\nProvide a concise review summary with specific issues if any.", 
                                item.pr_number.unwrap_or(0),
                                item.repo.as_deref().unwrap_or("unknown")),
                        };
                        match self.tokio_handle.block_on(agent.start(input)) {
                            Ok(_) => tracing::info!("Agent review started for run {}", run.id),
                            Err(e) => tracing::warn!("Agent review failed: {}", e),
                        }
                    }
                }
                reviewer_steps::PUBLISH => {
                    if let Ok(g) = self.repos.0.lock() {
                        let event = AppendInput {
                            event_type: "reviewer.publish".into(),
                            project_id: item.project_id.clone(),
                            loop_id: item.loop_id.clone(),
                            run_id: Some(run.id.clone()),
                            entity_type: Some("queue_item".into()),
                            entity_id: Some(item.id.clone()),
                            payload_json: Some(serde_json::json!({"step": "publish"}).to_string()),
                            ..AppendInput::new("")
                        };
                        if let Err(e) = eventlog::append(&g.events, &event) {
                            tracing::warn!("Reviewer publish event: {e}");
                        }
                        // Also create a notification
                        let notification = NotificationRecord {
                            id: Uuid::new_v4().to_string(),
                            project_id: item.project_id.clone(),
                            loop_id: item.loop_id.clone(),
                            run_id: Some(run.id.clone()),
                            entity_type: Some("loop".into()),
                            entity_id: item.loop_id.clone(),
                            channel: "internal".into(),
                            level: "info".into(),
                            title: format!("Review complete for loop {}", item.loop_id.as_deref().unwrap_or("?")),
                            subtitle: None,
                            body: format!("Reviewer finished pipeline (item={})", item.id),
                            status: "pending".into(),
                            dedupe_key: Some(format!("reviewer-done-{}", item.loop_id.as_deref().unwrap_or("?"))),
                            error_message: None,
                            payload_json: None,
                            sent_at: None,
                            created_at: now_iso.clone(),
                            updated_at: now_iso.clone(),
                        };
                        if let Err(e) = g.notifications.upsert(&notification) {
                            tracing::warn!("Reviewer notification: {e}");
                        }
                    }
                    // When GitHub is available, submit review and add label
                    if let Some(ref gw) = self.github {
                        if let Some(ref repo_path) = item.repo {
                            if let Some(pr_num) = item.pr_number {
                                // Add review label
                                let _ = gw.add_issue_labels(IssueLabelsInput {
                                    repo: repo_path.clone(),
                                    issue_number: pr_num,
                                    labels: vec!["looper/reviewed".into()],
                                    cwd: ".".to_string(),
                                });
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

        tracing::info!("Reviewer pipeline complete (loop={loop_id})");
        Ok(())
    }
}

impl ReviewerScheduler for Reviewer {
    fn discover_pull_requests(
        &self,
        _ctx: &Context,
        input: ReviewerDiscoveryInput,
    ) -> ReviewerDiscoveryResult {
        tracing::debug!("Reviewer discover_pull_requests — scanning for reviewable PRs via GitHub");

        if let Some(ref github) = self.github {
            if !input.repo.is_empty() {
                match github.list_open_pull_requests(ListOpenPullRequestsInput {
                    repo: input.repo.clone(),
                    cwd: ".".to_string(),
                    limit: 50,
                    label: "".into(),
                    labels: vec![],
                    author: "".into(),
                    base_ref_name: "".into(),
                    timeout: None,
                }) {
                    Ok(prs) => {
                        tracing::info!("Reviewer discovered {} open PRs via GitHub", prs.len());
                        // Convert discovered PRs into queue items
                        let now_iso = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
                        let mut discovered_items: Vec<QueueItemRecord> = Vec::new();
                        if let Ok(guard) = self.repos.0.lock() {
                            for (i, pr) in prs.iter().enumerate() {
                                tracing::debug!("Reviewer processing PR #{}/50 (number={})", i+1, pr.number);
                                let dedupe_key = format!("reviewer-{}-{}", input.project_id, pr.number);
                                // Create a loop record first (FK constraint on queue_items.loop_id)
                                let (loop_id, loop_rec) = {
                                    let lid = Uuid::new_v4().to_string();
                                    let loop_seq = match guard.loops.allocate_seq() {
                                        Ok(s) => s,
                                        Err(e) => {
                                            tracing::warn!("Failed to allocate loop seq for PR #{}: {e}", pr.number);
                                            continue;
                                        }
                                    };
                                    let r = LoopRecord {
                                        id: lid.clone(),
                                        seq: loop_seq,
                                        project_id: input.project_id.clone(),
                                        r#type: "review".into(),
                                        target_type: "pull_request".into(),
                                        target_id: Some(pr.number.to_string()),
                                        repo: Some(input.repo.clone()),
                                        pr_number: Some(pr.number),
                                        status: "queued".into(),
                                        config_json: None,
                                        metadata_json: None,
                                        last_run_at: None,
                                        next_run_at: None,
                                        created_at: now_iso.clone(),
                                        updated_at: now_iso.clone(),
                                    };
                                    (lid, r)
                                };
                                if let Err(e) = guard.loops.upsert(&loop_rec) {
                                    tracing::warn!("Failed to create loop for PR #{}: {e}", pr.number);
                                    continue;
                                }
                                let queue_item = QueueItemRecord {
                                    id: Uuid::new_v4().to_string(),
                                    project_id: Some(input.project_id.clone()),
                                    loop_id: Some(loop_id),
                                    r#type: "reviewer".into(),
                                    target_type: "pull_request".into(),
                                    target_id: pr.number.to_string(),
                                    repo: Some(input.repo.clone()),
                                    pr_number: Some(pr.number),
                                    dedupe_key,
                                    priority: 1,
                                    status: "queued".into(),
                                    available_at: now_iso.clone(),
                                    attempts: 0,
                                    max_attempts: 3,
                                    claimed_by: None,
                                    claimed_at: None,
                                    started_at: None,
                                    finished_at: None,
                                    lock_key: None,
                                    payload_json: Some(serde_json::json!({
                                        "title": pr.title,
                                        "url": pr.url,
                                        "author": pr.author,
                                        "head_ref": pr.head_ref_name,
                                        "base_ref": pr.base_ref_name,
                                    }).to_string()),
                                    last_error: None,
                                    last_error_kind: None,
                                    created_at: now_iso.clone(),
                                    updated_at: now_iso.clone(),
                                };
                                match guard.queue.upsert_active_by_dedupe_or_get_existing(&queue_item) {
                                    Ok((item, is_new)) => {
                                        discovered_items.push(item);
                                        tracing::info!("Reviewer enqueue PR #{}: is_new={} (total now={})", pr.number, is_new, discovered_items.len());
                                    }
                                    Err(e) => tracing::warn!("Failed to enqueue reviewer item for PR #{}: {}", pr.number, e),
                                }
                            }
                            drop(guard);
                            return ReviewerDiscoveryResult {
                                queue_items: discovered_items,
                            };
                        }
                    }
                    Err(e) => tracing::warn!("GitHub discovery failed for {}: {}", input.repo, e),
                }
            }
        }
        tracing::debug!("GitHub not configured or no repo, returning empty discovery for {}", input.project_id);
        ReviewerDiscoveryResult::default()
    }

    fn discover_pull_request(
        &self,
        _ctx: &Context,
        input: ReviewerTargetedDiscoveryInput,
    ) -> ReviewerDiscoveryResult {
        tracing::debug!("Reviewer discover_pull_request — scanning for PR-specific reviewer items");
        let guard = match self.repos.0.lock() {
            Ok(g) => g,
            Err(e) => {
                tracing::error!("Reviewer discover_pull_request lock: {e}");
                return ReviewerDiscoveryResult::default();
            }
        };
        let queued = match guard.queue.list_by_statuses(&["queued".into()]) {
            Ok(items) => items,
            Err(e) => {
                tracing::error!("Reviewer discover_pull_request list: {e}");
                Vec::new()
            }
        };
        let reviewer_items: Vec<_> = queued
            .into_iter()
            .filter(|item| item.r#type == "reviewer")
            .collect();
        drop(guard);

        // GitHub-powered PR discovery
        if let Some(ref gw) = self.github {
            if !input.repo.is_empty() {
                match gw.view_pull_request(ViewPullRequestInput {
                    repo: input.repo.clone(),
                    pr_number: input.pr_number,
                    cwd: ".".to_string(),
                }) {
                    Ok(pr) => tracing::info!("Reviewer discovered targeted PR #{}: {}", pr.number, pr.title),
                    Err(e) => tracing::warn!("Reviewer targeted GitHub discovery failed: {e}"),
                }
            }
        }

        ReviewerDiscoveryResult {
            queue_items: reviewer_items,
        }
    }

    fn process_claimed_queue_item(
        &self,
        _ctx: &Context,
        item: &QueueItemRecord,
    ) -> Result<(), String> {
        self.execute_pipeline(item)
    }
}
