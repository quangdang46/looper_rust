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
use looper_config::types::DisclosureConfig;
use looper_git::types::{CheckoutMode, CleanupWorktreeInput, CreateWorktreeInput};
use looper_git::{build_worktree_directory_name, Gateway as GitGateway};

use regex::Regex;
use crate::reviewer_criteria;
use crate::reviewer_criteria::{DefaultVerifier, DiffFile, PRDiff};
use crate::types::{reviewer_steps, spec_labels, SpecPhase};
use looper_github::types::{
    GetPullRequestDiffInput, IssueAssigneesInput, IssueCommentInput, IssueLabelsInput, ListOpenPullRequestsInput,
    ReviewComment, SubmitReviewInput, ViewPullRequestInput,
};
use looper_scheduler::scheduler::SendRepos;
use looper_scheduler::types::{
    Context, ReviewerDiscoveryInput, ReviewerDiscoveryResult, ReviewerScheduler, ReviewerTargetedDiscoveryInput,
    SchedulerConfig,
};
use looper_storage::eventlog;
use looper_storage::record::{
    AppendInput, LoopRecord, NotificationRecord, PullRequestSnapshotRecord, QueueItemRecord, RunRecord,
};
use looper_types::RunStatus;

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
        Self { config: config.clone(), repos, github, tokio_handle, agent, git }
    }

    /// Shared pipeline logic used by both single-PR and bulk discovery paths.
    fn execute_pipeline(&self, item: &QueueItemRecord) -> Result<(), String> {
        let ctx = Context::new();
        let now_iso = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();

        let loop_id = item.loop_id.as_deref().ok_or_else(|| "Reviewer queue item has no loop_id".to_string())?;

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
                    // Check PR phase — only review spec-phase PRs (those with
                    // looper:spec-reviewing label). Skip implementation PRs.
                    if let Some(ref gw) = self.github {
                        if let Some(ref repo_path) = item.repo {
                            if let Some(pr_num) = item.pr_number {
                                match gw.view_pull_request(ViewPullRequestInput {
                                    repo: repo_path.clone(),
                                    pr_number: pr_num,
                                    cwd: ".".to_string(),
                                }) {
                                    Ok(pr) => {
                                        let phase = SpecPhase::from_labels(&pr.labels);
                                        match phase {
                                            SpecPhase::SpecReviewing => {
                                                tracing::info!("Reviewer: PR #{} is in spec phase, proceeding", pr_num);
                                            }
                                            SpecPhase::SpecReady => {
                                                tracing::info!(
                                                    "Reviewer: PR #{} spec already approved, skipping",
                                                    pr_num
                                                );
                                                continue;
                                            }
                                            SpecPhase::NeedsHuman => {
                                                tracing::info!("Reviewer: PR #{} needs human, skipping", pr_num);
                                                continue;
                                            }
                                            SpecPhase::Unknown => {
                                                tracing::debug!("Reviewer: PR #{} has no spec label, treating as implementation review", pr_num);
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!("Reviewer: failed to view PR #{} for phase check: {e}", pr_num);
                                    }
                                }
                            }
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
                    // Store diff snapshot for review-round comparison
                    if let Some(ref gw) = self.github {
                        if let Some(ref repo_path) = item.repo {
                            if let Some(pr_num) = item.pr_number {
                                match gw.view_pull_request(ViewPullRequestInput {
                                    repo: repo_path.clone(),
                                    pr_number: pr_num,
                                    cwd: ".".to_string(),
                                }) {
                                    Ok(pr) => {
                                        let diff = gw
                                            .get_pull_request_diff(GetPullRequestDiffInput {
                                                repo: repo_path.clone(),
                                                pr_number: pr_num,
                                                cwd: ".".to_string(),
                                            })
                                            .unwrap_or_default();
                                        if let Ok(guard) = self.repos.0.lock() {
                                            let snapshot = PullRequestSnapshotRecord {
                                                id: Uuid::new_v4().to_string(),
                                                project_id: item.project_id.clone().unwrap_or_default(),
                                                repo: repo_path.clone(),
                                                pr_number: pr_num,
                                                head_sha: pr.head_sha.clone(),
                                                base_sha: Some(pr.base_sha.clone()),
                                                title: Some(pr.title.clone()),
                                                body: Some(pr.body.clone()),
                                                author: Some(pr.author.clone()),
                                                diff_ref: Some(diff),
                                                checks_summary: None,
                                                unresolved_thread_count: None,
                                                review_state: None,
                                                payload_json: None,
                                                captured_at: now_iso.clone(),
                                                created_at: now_iso.clone(),
                                            };
                                            if let Err(e) = guard.pull_request_snapshots.upsert(&snapshot) {
                                                tracing::warn!("Reviewer snapshot store failed: {e}");
                                            }
                                            drop(guard);
                                        }
                                    }
                                    Err(e) => tracing::warn!("Reviewer snapshot PR view failed: {e}"),
                                }
                            }
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
                            Ok(_) => {
                                tracing::info!("Worktree created for project {}", project_id.as_deref().unwrap_or("?"))
                            }
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

                    // Check if the diff has changed since the last review round.
                    // If it's the same head_sha as the latest snapshot, skip re-review.
                    let mut _diff_changed = true;
                    if let Some(ref repo_path) = item.repo {
                        if let Some(pr_num) = item.pr_number {
                            if let Ok(guard) = self.repos.0.lock() {
                                if let Some(latest) = guard
                                    .pull_request_snapshots
                                    .get_latest_by_project(
                                        &item.project_id.clone().unwrap_or_default(),
                                        repo_path,
                                        pr_num,
                                    )
                                    .unwrap_or(None)
                                {
                                    // If there are at least 2 snapshots and the newest head_sha matches
                                    // the previous one, the diff hasn't changed.
                                    if let Some(ref gw) = self.github {
                                        if let Ok(pr) = gw.view_pull_request(ViewPullRequestInput {
                                            repo: repo_path.clone(),
                                            pr_number: pr_num,
                                            cwd: ".".to_string(),
                                        }) {
                                            if latest.head_sha == pr.head_sha {
                                                _diff_changed = false;
                                                tracing::info!("Reviewer: PR #{} diff unchanged (head_sha={}), re-using previous review", pr_num, pr.head_sha);
                                            }
                                        }
                                    }
                                }
                                drop(guard);
                            }
                        }
                    }
                    if let Some(ref agent) = self.agent {
                        let effective_path = if item.project_id.clone().unwrap_or_default().is_empty() {
                            item.repo.clone().unwrap_or_default()
                        } else {
                            format!("/tmp/e2e-{}", item.project_id.as_deref().unwrap_or("looper"))
                        };
                        let worktree_dir =
                            looper_git::build_worktree_directory_name(&looper_git::CreateWorktreeInput {
                                project_id: item.project_id.clone().unwrap_or_default(),
                                repo_path: String::new(),
                                worktree_root: String::new(),
                                branch: format!("review/{}", run.loop_id),
                                base_branch: None,
                                start_point: None,
                                pr_number: None,
                                checkout_mode: CheckoutMode::Branch,
                                protected_branches: vec![],
                            });
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
                            Ok(exec) => {
                                tracing::info!("Agent review started for run {}", run.id);
                                let agent_result = self.tokio_handle.block_on(exec.wait());
                                match &agent_result {
                                    Ok(result) => {
                                        tracing::info!(
                                            "Agent review completed for run {}: status={}",
                                            run.id,
                                            result.status
                                        );
                                        // Store agent stdout for inline comment parsing in PUBLISH step
                                        if let Ok(guard) = self.repos.0.lock() {
                                            let mut r =
                                                guard.runs.get_by_id(&run.id).unwrap_or(None).unwrap_or(run.clone());
                                            if let Some(ref existing) = r.checkpoint_json {
                                                if let Ok(mut cp) = serde_json::from_str::<serde_json::Value>(existing)
                                                {
                                                    cp["agent_stdout"] =
                                                        serde_json::Value::String(result.stdout.clone());
                                                    r.checkpoint_json = Some(cp.to_string());
                                                    let _ = guard.runs.upsert(&r);
                                                }
                                            } else {
                                                let cp = serde_json::json!({"agent_stdout": result.stdout, "agent_summary": result.summary});
                                                r.checkpoint_json = Some(cp.to_string());
                                                let _ = guard.runs.upsert(&r);
                                            }
                                            drop(guard);
                                        }
                                    }
                                    Err(e) => tracing::warn!("Agent review wait failed for run {}: {}", run.id, e),
                                }
                            }
                            Err(e) => tracing::warn!("Agent review failed: {}", e),
                        }
                    }

                    // After agent review, verify acceptance criteria against the diff
                    if let Some(ref gw) = self.github {
                        if let Some(ref repo_path) = item.repo {
                            if let Some(pr_num) = item.pr_number {
                                if let Ok(pr) = gw.view_pull_request(ViewPullRequestInput {
                                    repo: repo_path.clone(),
                                    pr_number: pr_num,
                                    cwd: ".".to_string(),
                                }) {
                                    // Extract acceptance criteria from PR body
                                    let criteria = reviewer_criteria::extract(&pr.body);
                                    if !criteria.is_empty() {
                                        tracing::info!(
                                            "Reviewer: extracted {} acceptance criteria from PR #{}",
                                            criteria.len(),
                                            pr_num
                                        );
                                        // Get the PR diff and convert to PRDiff
                                        let diff = gw
                                            .get_pull_request_diff(GetPullRequestDiffInput {
                                                repo: repo_path.clone(),
                                                pr_number: pr_num,
                                                cwd: ".".to_string(),
                                            })
                                            .unwrap_or_default();
                                        let pr_diff = PRDiff {
                                            files: vec![DiffFile {
                                                path: "full_diff".to_string(),
                                                patch: diff.clone(),
                                            }],
                                        };
                                        // Verify criteria against diff
                                        let verifier = DefaultVerifier::new();
                                        match reviewer_criteria::verify(&criteria, &pr_diff, &verifier) {
                                            Ok(result) => {
                                                let pass_count = result
                                                    .criteria
                                                    .iter()
                                                    .filter(|c| c.verdict == reviewer_criteria::Verdict::Pass)
                                                    .count();
                                                let fail_count = result
                                                    .criteria
                                                    .iter()
                                                    .filter(|c| c.verdict == reviewer_criteria::Verdict::Fail)
                                                    .count();
                                                let unverifiable_count = result
                                                    .criteria
                                                    .iter()
                                                    .filter(|c| c.verdict == reviewer_criteria::Verdict::Unverifiable)
                                                    .count();
                                                tracing::info!(
                                                    "Reviewer: criteria verification — pass={}, fail={}, unverifiable={}",
                                                    pass_count, fail_count, unverifiable_count
                                                );
                                                // Store verification result as checkpoint so the PUBLISH step can use it
                                                if let Ok(guard) = self.repos.0.lock() {
                                                    let mut r = guard
                                                        .runs
                                                        .get_by_id(&run.id)
                                                        .unwrap_or(None)
                                                        .unwrap_or(run.clone());
                                                    r.checkpoint_json = Some(
                                                        serde_json::json!({
                                                            "criteria_disposition": format!("{:?}", result.disposition),
                                                            "criteria_pass": pass_count,
                                                            "criteria_fail": fail_count,
                                                            "criteria_unverifiable": unverifiable_count,
                                                            "criteria_total": result.criteria.len(),
                                                        })
                                                        .to_string(),
                                                    );
                                                    let _ = guard.runs.upsert(&r);
                                                    drop(guard);
                                                }
                                            }
                                            Err(e) => tracing::warn!("Reviewer: criteria verification failed: {e}"),
                                        }
                                    }
                                }
                            }
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
                    // When GitHub is available, submit review, post comments, and add label
                    if let Some(ref gw) = self.github {
                        if let Some(ref repo_path) = item.repo {
                            if let Some(pr_num) = item.pr_number {
                                // Collect criteria verification result from checkpoint
                                let criteria_info = {
                                    let guard = self.repos.0.lock().ok();
                                    guard
                                        .as_ref()
                                        .and_then(|g| g.runs.get_by_id(&run.id).ok().flatten())
                                        .and_then(|r| r.checkpoint_json)
                                        .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok())
                                };

                                // Build review body including criteria verification results
                                let mut review_body = String::from("## Looper Review\n\n*Automated review by looper.*\n");

                                // Include agent review summary from checkpoint
                                if let Some(ref info) = criteria_info {
                                    if let Some(summary) = info.get("agent_stdout").and_then(|v| v.as_str()) {
                                        if !summary.is_empty() {
                                            // Strip __LOOPER_RESULT__ line and ANSI escape codes
                                            let ansi_re = Regex::new(r"\x1b\[[0-9;]*[a-zA-Z~]").unwrap();
                                            let clean_summary: String = summary
                                                .lines()
                                                .filter(|l| !l.contains("__LOOPER_RESULT__="))
                                                .map(|l| ansi_re.replace_all(l, "").to_string())
                                                .filter(|l| !l.trim().is_empty())
                                                .collect::<Vec<_>>()
                                                .join("\n");
                                            if !clean_summary.trim().is_empty() {
                                                review_body.push_str(&format!("\n---\n{}\n", clean_summary));
                                            }
                                        }
                                    }
                                }
                                let mut has_issues = false;
                                if let Some(ref info) = criteria_info {
                                    if let Some(total) = info.get("criteria_total").and_then(|v| v.as_u64()) {
                                        if total > 0 {
                                            let pass = info.get("criteria_pass").and_then(|v| v.as_u64()).unwrap_or(0);
                                            let fail = info.get("criteria_fail").and_then(|v| v.as_u64()).unwrap_or(0);
                                            let unver =
                                                info.get("criteria_unverifiable").and_then(|v| v.as_u64()).unwrap_or(0);
                                            review_body.push_str(&format!(
                                                "\n### Acceptance Criteria Check\n- Pass: {}\n- Fail: {}\n- Unverifiable: {}\n",
                                                pass, fail, unver
                                            ));
                                            if fail > 0 || unver > 0 {
                                                has_issues = true;
                                            }
                                        }
                                    }
                                }

                                // Submit a formal PR review (not just labels)
                                let event = if has_issues { "COMMENT" } else { "APPROVE" };
                                let pr_head_sha = gw
                                    .view_pull_request(ViewPullRequestInput {
                                        repo: repo_path.clone(),
                                        pr_number: pr_num,
                                        cwd: ".".to_string(),
                                    })
                                    .ok()
                                    .map(|pr| pr.head_sha)
                                    .unwrap_or_default();
                                // Parse inline review comments from agent stdout
                                let inline_comments = extract_inline_comments_from_checkpoint(item, &self.repos);
                                let _ = gw.submit_review(SubmitReviewInput {
                                    repo: repo_path.clone(),
                                    pr_number: pr_num,
                                    event: event.to_string(),
                                    body: review_body.clone(),
                                    commit_id: pr_head_sha,
                                    comments: inline_comments,
                                    anchors: None,
                                    disclosure: DisclosureConfig::default(),
                                    cwd: ".".to_string(),
                                });
                                tracing::info!("Reviewer: submitted PR review ({}) for PR #{}", event, pr_num);

                                // Post review summary as PR comment for easy checking
                                if !review_body.is_empty() {
                                                                    // Add "Generated by Looper" link at the end
                                let looper_link = format!(
                                    "
---
*🤖 Generated by [Looper](https://github.com/quangdang46/looper_rust)*"
                                );
                                review_body.push_str(&looper_link);
                                let _ = gw.create_issue_comment(IssueCommentInput {
                                        repo: repo_path.clone(),
                                        issue_number: pr_num,
                                        body: format!(
                                            "## 🔍 Review Result ({})

{}",
                                            event, review_body
                                        ),
                                        cwd: ".".to_string(),
                                    });
                                    tracing::info!("Reviewer: posted review comment to PR #{}", pr_num);
                                }

                                // Add review label
                                let _ = gw.add_issue_labels(IssueLabelsInput {
                                    repo: repo_path.clone(),
                                    issue_number: pr_num,
                                    labels: vec!["looper/reviewed".into()],
                                    cwd: ".".to_string(),
                                });

                                // Transition spec-phase label based on review outcome
                                if let Ok(pr) = gw.view_pull_request(ViewPullRequestInput {
                                    repo: repo_path.clone(),
                                    pr_number: pr_num,
                                    cwd: ".".to_string(),
                                }) {
                                    let has_spec_reviewing = pr.labels.iter().any(|l| l == spec_labels::SPEC_REVIEWING);
                                    if has_spec_reviewing {
                                        let target =
                                            if has_issues { spec_labels::NEEDS_HUMAN } else { spec_labels::SPEC_READY };
                                        let _ = gw.add_issue_labels(IssueLabelsInput {
                                            repo: repo_path.clone(),
                                            issue_number: pr_num,
                                            labels: vec![target.to_string()],
                                            cwd: ".".to_string(),
                                        });
                                        // Mark PR ready for review since spec has been reviewed
                                        if target == spec_labels::SPEC_READY {
                                            let _ = gw.mark_pr_ready(
                                                looper_github::types::MarkPullRequestReadyForReviewInput {
                                                    repo: repo_path.clone(),
                                                    pr_number: pr_num,
                                                    cwd: ".".to_string(),
                                                },
                                            );
                                        }
                                        tracing::info!("Reviewer: PR #{} spec transitioned to {}", pr_num, target);
                                    } else {
                                        // Not a spec PR — code implementation PR.
                                        // PR is ready for human review — no auto-merge.
                                        tracing::info!(
                                            "Reviewer: PR #{} approved — ready for human review and manual merge",
                                            pr_num
                                        );
                                    }
                                }
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
            let wt_dir = build_worktree_directory_name(&CreateWorktreeInput {
                project_id: item.project_id.clone().unwrap_or_default(),
                repo_path: ".".to_string(),
                worktree_root: ".".to_string(),
                branch: format!("review/{}", &run.loop_id),
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
                branch: format!("review/{}", &run.loop_id),
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

        tracing::info!("Reviewer pipeline complete (loop={loop_id})");
        Ok(())
    }
}

impl ReviewerScheduler for Reviewer {
    fn discover_pull_requests(&self, _ctx: &Context, input: ReviewerDiscoveryInput) -> ReviewerDiscoveryResult {
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
                            // Pre-fetch existing active loops for this project to avoid duplicates
                            let existing_loops: Vec<LoopRecord> = match guard.loops.list() {
                                Ok(all_loops) => all_loops
                                    .into_iter()
                                    .filter(|l| {
                                        l.project_id == input.project_id
                                            && l.target_type == "pull_request"
                                            && !matches!(l.status.as_str(), "completed" | "failed" | "cancelled" | "terminated")
                                    })
                                    .collect(),
                                Err(_) => Vec::new(),
                            };
                            for (i, pr) in prs.iter().enumerate() {
                                tracing::debug!("Reviewer processing PR #{}/50 (number={})", i + 1, pr.number);
                                let dedupe_key = format!("reviewer-{}-{}", input.project_id, pr.number);
                                // Check if a loop already exists for this PR
                                if existing_loops.iter().any(|l| l.pr_number == Some(pr.number)) {
                                    tracing::debug!("Reviewer: loop already exists for PR #{}, skipping loop creation", pr.number);
                                    // Still need to ensure a queue item exists (upsert handles dedup)
                                    // Find the existing loop to reference
                                    if let Some(existing_loop) = existing_loops.iter().find(|l| l.pr_number == Some(pr.number)) {
                                        let loop_id = existing_loop.id.clone();
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
                                            payload_json: Some(
                                                serde_json::json!({
                                                    "title": pr.title,
                                                    "url": pr.url,
                                                    "author": pr.author,
                                                    "head_ref": pr.head_ref_name,
                                                    "base_ref": pr.base_ref_name,
                                                })
                                                .to_string(),
                                            ),
                                            last_error: None,
                                            last_error_kind: None,
                                            created_at: now_iso.clone(),
                                            updated_at: now_iso.clone(),
                                        };
                                        match guard.queue.upsert_active_by_dedupe_or_get_existing(&queue_item) {
                                            Ok((item, is_new)) => {
                                                discovered_items.push(item);
                                                tracing::info!(
                                                    "Reviewer enqueue PR #{}: is_new={} (total now={})",
                                                    pr.number,
                                                    is_new,
                                                    discovered_items.len()
                                                );
                                            }
                                            Err(e) => tracing::warn!("Failed to enqueue reviewer item for PR #{}: {}", pr.number, e),
                                        }
                                    }
                                    continue;
                                }
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
                                    payload_json: Some(
                                        serde_json::json!({
                                            "title": pr.title,
                                            "url": pr.url,
                                            "author": pr.author,
                                            "head_ref": pr.head_ref_name,
                                            "base_ref": pr.base_ref_name,
                                        })
                                        .to_string(),
                                    ),
                                    last_error: None,
                                    last_error_kind: None,
                                    created_at: now_iso.clone(),
                                    updated_at: now_iso.clone(),
                                };
                                match guard.queue.upsert_active_by_dedupe_or_get_existing(&queue_item) {
                                    Ok((item, is_new)) => {
                                        discovered_items.push(item);
                                        tracing::info!(
                                            "Reviewer enqueue PR #{}: is_new={} (total now={})",
                                            pr.number,
                                            is_new,
                                            discovered_items.len()
                                        );
                                    }
                                    Err(e) => {
                                        tracing::warn!("Failed to enqueue reviewer item for PR #{}: {}", pr.number, e)
                                    }
                                }
                            }
                            drop(guard);
                            return ReviewerDiscoveryResult { queue_items: discovered_items };
                        }
                    }
                    Err(e) => tracing::warn!("GitHub discovery failed for {}: {}", input.repo, e),
                }
            }
        }
        tracing::debug!("GitHub not configured or no repo, returning empty discovery for {}", input.project_id);
        ReviewerDiscoveryResult::default()
    }

    fn discover_pull_request(&self, _ctx: &Context, input: ReviewerTargetedDiscoveryInput) -> ReviewerDiscoveryResult {
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
        let reviewer_items: Vec<_> = queued.into_iter().filter(|item| item.r#type == "reviewer").collect();
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

        ReviewerDiscoveryResult { queue_items: reviewer_items }
    }

    fn process_claimed_queue_item(&self, _ctx: &Context, item: &QueueItemRecord) -> Result<(), String> {
        self.execute_pipeline(item)
    }
}

/// Parse inline review comments from the agent's stdout stored in the run checkpoint.
fn extract_inline_comments_from_checkpoint(item: &QueueItemRecord, repos: &Arc<SendRepos>) -> Vec<ReviewComment> {
    let agent_stdout = repos
        .0
        .lock()
        .ok()
        .and_then(|g| {
            let loop_id = item.loop_id.as_deref()?;
            g.runs.get_latest_by_loop_id(loop_id).ok().flatten()
        })
        .and_then(|r| {
            let cp: serde_json::Value = serde_json::from_str(&r.checkpoint_json?).ok()?;
            cp.get("agent_stdout")?.as_str().map(|s| s.to_string())
        });

    let Some(ref stdout) = agent_stdout else { return vec![] };

    let mut comments = Vec::new();
    let re = Regex::new(
        r"(?s)```review-comment\s*
path:\s*([^
]+)
line:\s*(\d+)
body:\s*(.*?)```",
    )
    .unwrap();
    for cap in re.captures_iter(stdout) {
        let path = cap.get(1).map(|m| m.as_str().trim().to_string()).unwrap_or_default();
        let line: i64 = cap.get(2).and_then(|m| m.as_str().parse().ok()).unwrap_or(0);
        let body = cap.get(3).map(|m| m.as_str().trim().to_string()).unwrap_or_default();
        if !path.is_empty() && !body.is_empty() {
            comments.push(ReviewComment {
                body,
                path,
                line,
                side: "RIGHT".to_string(),
                start_line: 0,
                start_side: String::new(),
                diagnostic_index: 0,
            });
        }
    }
    if !comments.is_empty() {
        tracing::info!("Reviewer: parsed {} inline comment(s) from agent output", comments.len());
    }
    comments
}
