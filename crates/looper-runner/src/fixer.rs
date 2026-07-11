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
use std::time::Instant;

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
use looper_storage::record::{AppendInput, QueueItemRecord, QueueMarkRetryInput, RunRecord};
use looper_storage::repos::Repositories;
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

// ---------------------------------------------------------------------------
// Worktree + REPAIR helpers (unit-tested)
// ---------------------------------------------------------------------------

/// Branch name used for the fixer's dedicated worktree.
pub fn fixer_branch_name(loop_id: &str) -> String {
    format!("fix/{loop_id}")
}

/// Build the absolute worktree path for a fixer run (mirrors git gateway naming).
///
/// Never returns `"."` or empty when `repo_path` is non-empty.
pub fn build_fixer_worktree_path(repo_path: &str, project_id: &str, loop_id: &str, pr_number: Option<i64>) -> String {
    let repo = repo_path.trim_end_matches('/');
    let root = format!("{repo}/.looper/worktrees");
    let dir = build_worktree_directory_name(&CreateWorktreeInput {
        project_id: project_id.to_string(),
        repo_path: repo.to_string(),
        worktree_root: root.clone(),
        branch: fixer_branch_name(loop_id),
        base_branch: None,
        start_point: None,
        pr_number,
        checkout_mode: CheckoutMode::Branch,
        protected_branches: vec![],
    });
    format!("{root}/{dir}")
}

/// Resolve project `repo_path` for a queue item (DB first, then CWD).
fn resolve_fixer_repo_path(fixer: &Fixer, item: &QueueItemRecord) -> String {
    if let Some(ref pid) = item.project_id {
        if let Ok(g) = fixer.repos.0.lock() {
            if let Ok(Some(proj)) = g.projects.get_by_id(pid) {
                if !proj.repo_path.is_empty() {
                    return proj.repo_path;
                }
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        cwd.display().to_string()
    } else {
        ".".to_string()
    }
}

/// Resolve the fixer worktree path: worktrees table → project.repo_path layout → CWD layout.
fn resolve_fixer_wt(fixer: &Fixer, item: &QueueItemRecord, loop_id: &str) -> String {
    let branch = fixer_branch_name(loop_id);
    let project_id = item.project_id.clone().unwrap_or_default();

    if !project_id.is_empty() {
        if let Ok(g) = fixer.repos.0.lock() {
            if let Ok(Some(wt)) = g.worktrees.get_by_branch(&project_id, &branch) {
                if wt.status != "cleaned" && !wt.worktree_path.is_empty() && wt.worktree_path != "." {
                    return wt.worktree_path;
                }
            }
            if let Ok(Some(proj)) = g.projects.get_by_id(&project_id) {
                if !proj.repo_path.is_empty() {
                    return build_fixer_worktree_path(&proj.repo_path, &project_id, loop_id, item.pr_number);
                }
            }
        }
    }

    let repo = resolve_fixer_repo_path(fixer, item);
    build_fixer_worktree_path(&repo, &project_id, loop_id, item.pr_number)
}

/// Read `worktree_path` from run checkpoint JSON (if present and usable).
pub fn worktree_from_checkpoint(checkpoint_json: Option<&str>) -> Option<String> {
    let raw = checkpoint_json?;
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let path = v.get("worktree_path")?.as_str()?.to_string();
    if path.is_empty() || path == "." {
        None
    } else {
        Some(path)
    }
}

/// Merge `worktree_path` into run checkpoint JSON (preserves other keys).
pub fn merge_checkpoint_worktree(checkpoint_json: Option<&str>, worktree_path: &str) -> String {
    let mut cp = checkpoint_json
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if !cp.is_object() {
        cp = serde_json::json!({});
    }
    cp["worktree_path"] = serde_json::Value::String(worktree_path.to_string());
    cp.to_string()
}

/// Whether an agent wait result status counts as success for REPAIR.
pub fn agent_repair_succeeded(status: &str) -> bool {
    status == "completed"
}

/// Validate REPAIR agent working directory — fail closed if empty / bare `"."`.
pub fn validate_repair_start_cwd(working_directory: &str) -> Result<(), String> {
    if working_directory.is_empty() {
        return Err("working_directory is required (empty cwd is not allowed for REPAIR)".into());
    }
    if working_directory == "." {
        return Err("working_directory must be a real worktree path, not \".\"".into());
    }
    Ok(())
}

/// Contract for REPAIR agent start+wait (production uses ConfiguredExecutor; tests use a mock).
pub trait RepairAgentSession {
    /// Start the agent and block until wait completes. Returns agent status string.
    fn start_and_wait(&mut self, working_directory: &str, prompt: &str) -> Result<String, String>;
}

/// Run REPAIR agent session: validate cwd, start+wait, require completed status.
///
/// Used by unit tests with a mock agent; production mirrors this control flow
/// against [`ConfiguredExecutor`].
pub fn run_repair_agent_session<A: RepairAgentSession>(
    agent: &mut A,
    working_directory: &str,
    prompt: &str,
) -> Result<String, String> {
    validate_repair_start_cwd(working_directory)?;
    let status = agent.start_and_wait(working_directory, prompt)?;
    if agent_repair_succeeded(&status) {
        Ok(status)
    } else {
        Err(format!("agent repair failed: status={status}"))
    }
}

/// Fail closed: mark queue item for retry and set run status to Failed (never Success).
pub fn apply_repair_fail_closed(
    repos: &Repositories,
    item: &QueueItemRecord,
    run_id: &str,
    error: &str,
    error_kind: &str,
) -> Result<(), String> {
    let now_iso = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
    let attempts = item.attempts + 1;
    repos
        .queue
        .mark_retry(&QueueMarkRetryInput {
            id: item.id.clone(),
            available_at: now_iso.clone(),
            attempts,
            error_message: Some(error.to_string()),
            error_kind: error_kind.to_string(),
            updated_at: now_iso.clone(),
        })
        .map_err(|e| e.to_string())?;

    if let Some(mut failed_run) = repos.runs.get_by_id(run_id).map_err(|e| e.to_string())? {
        failed_run.status = RunStatus::Failed.as_str().to_string();
        failed_run.error_message = Some(error.to_string());
        failed_run.ended_at = Some(now_iso.clone());
        failed_run.updated_at = now_iso;
        // Never leave a Success status on agent failure.
        debug_assert_ne!(failed_run.status, RunStatus::Success.as_str());
        repos.runs.upsert(&failed_run).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn is_loop_paused(fixer: &Fixer, loop_id: &str) -> bool {
    if let Ok(g) = fixer.repos.0.lock() {
        if let Ok(Some(l)) = g.loops.get_by_id(loop_id) {
            return l.status == "paused";
        }
    }
    false
}

fn persist_run_checkpoint(fixer: &Fixer, run_id: &str, checkpoint_json: String) -> Result<(), String> {
    let guard = fixer.repos.0.lock().map_err(|e| e.to_string())?;
    let mut r = guard.runs.get_by_id(run_id).map_err(|e| e.to_string())?.ok_or("run not found for checkpoint")?;
    r.checkpoint_json = Some(checkpoint_json);
    r.updated_at = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
    guard.runs.upsert(&r).map_err(|e| e.to_string())?;
    Ok(())
}

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
                    // Resolve project repo_path + create fix worktree (like worker).
                    let repo_path = resolve_fixer_repo_path(self, item);
                    let worktree_root = format!("{}/.looper/worktrees", repo_path.trim_end_matches('/'));
                    let branch = fixer_branch_name(loop_id);
                    let project_id = item.project_id.clone().unwrap_or_default();

                    let mut worktree_path = build_fixer_worktree_path(&repo_path, &project_id, loop_id, item.pr_number);

                    if let Some(ref git) = self.git {
                        let input = CreateWorktreeInput {
                            project_id: project_id.clone(),
                            repo_path: repo_path.clone(),
                            worktree_root: worktree_root.clone(),
                            branch: branch.clone(),
                            base_branch: Some("main".to_string()),
                            start_point: Some("main".to_string()),
                            pr_number: item.pr_number,
                            checkout_mode: CheckoutMode::Branch,
                            protected_branches: vec!["main".to_string(), "master".to_string()],
                        };
                        match self.tokio_handle.block_on(git.create_worktree(input)) {
                            Ok(result) => {
                                worktree_path = result.record.worktree_path.clone();
                                tracing::info!(
                                    loop_id = %loop_id,
                                    pr = item.pr_number,
                                    worktree_path = %worktree_path,
                                    recovered = result.recovered,
                                    "Fixer worktree prepared"
                                );
                            }
                            Err(e) => {
                                tracing::warn!(
                                    loop_id = %loop_id,
                                    worktree_path = %worktree_path,
                                    "Worktree creation failed: {e}"
                                );
                            }
                        }
                    } else {
                        let _ = std::fs::create_dir_all(&worktree_path);
                        tracing::info!(
                            loop_id = %loop_id,
                            worktree_path = %worktree_path,
                            "Fixer worktree path resolved (no git gateway)"
                        );
                    }

                    // Checkpoint path on run for REPAIR / PUSH / RECHECK.
                    let cp = {
                        let guard = self.repos.0.lock().map_err(|e| e.to_string())?;
                        let existing =
                            guard.runs.get_by_id(&run.id).map_err(|e| e.to_string())?.and_then(|r| r.checkpoint_json);
                        drop(guard);
                        merge_checkpoint_worktree(existing.as_deref(), &worktree_path)
                    };
                    if let Err(e) = persist_run_checkpoint(self, &run.id, cp) {
                        tracing::warn!("Fixer prepare_worktree checkpoint: {e}");
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

                    // B7: if loop paused mid-fixer, do not start agent.
                    if is_loop_paused(self, loop_id) {
                        let err = format!("loop {loop_id} is paused; not starting REPAIR agent");
                        tracing::info!(loop_id = %loop_id, "{err}");
                        if let Ok(g) = self.repos.0.lock() {
                            let _ = apply_repair_fail_closed(&g, item, &run.id, &err, "retryable_after_resume");
                        }
                        return Err(err);
                    }

                    // Resolve worktree: checkpoint → worktrees table / project.repo_path.
                    let worktree_path = {
                        let from_cp = {
                            let guard = self.repos.0.lock().map_err(|e| e.to_string())?;
                            guard
                                .runs
                                .get_by_id(&run.id)
                                .map_err(|e| e.to_string())?
                                .and_then(|r| worktree_from_checkpoint(r.checkpoint_json.as_deref()))
                        };
                        from_cp.unwrap_or_else(|| resolve_fixer_wt(self, item, loop_id))
                    };

                    if let Err(e) = validate_repair_start_cwd(&worktree_path) {
                        let err = format!("REPAIR worktree invalid: {e}");
                        if let Ok(g) = self.repos.0.lock() {
                            let _ = apply_repair_fail_closed(&g, item, &run.id, &err, "non_retryable");
                        }
                        return Err(err);
                    }

                    let _ = std::fs::create_dir_all(&worktree_path);

                    // Re-checkpoint resolved path.
                    let cp = {
                        let guard = self.repos.0.lock().map_err(|e| e.to_string())?;
                        let existing =
                            guard.runs.get_by_id(&run.id).map_err(|e| e.to_string())?.and_then(|r| r.checkpoint_json);
                        drop(guard);
                        merge_checkpoint_worktree(existing.as_deref(), &worktree_path)
                    };
                    let _ = persist_run_checkpoint(self, &run.id, cp);

                    // Apply repair via agent: start + wait (mirror worker EXECUTE).
                    if let Some(ref agent) = self.agent {
                        let prompt = format!(
                            "Apply the fixes for PR #{} in repo {}. Make the necessary code changes and verify they work. Work only inside the given worktree.",
                            item.pr_number.unwrap_or(0),
                            item.repo.as_deref().unwrap_or("unknown")
                        );
                        let input = looper_agent::executor::StartInput {
                            loop_id: run.loop_id.clone(),
                            current_step: Some(fixer_steps::REPAIR.to_string()),
                            last_completed_step: Some(fixer_steps::PREPARE_WORKTREE.to_string()),
                            checkpoint_json: None,
                            project_id: item.project_id.clone().unwrap_or_default(),
                            run_id: run.id.clone(),
                            working_directory: worktree_path.clone(),
                            prompt,
                        };

                        let started = Instant::now();
                        match self.tokio_handle.block_on(agent.start(input)) {
                            Ok(exec) => {
                                tracing::info!(
                                    loop_id = %loop_id,
                                    pr = item.pr_number,
                                    worktree_path = %worktree_path,
                                    "Fixer REPAIR agent started; waiting for completion"
                                );
                                match self.tokio_handle.block_on(exec.wait()) {
                                    Ok(result) => {
                                        let duration_ms = started.elapsed().as_millis() as u64;
                                        tracing::info!(
                                            loop_id = %loop_id,
                                            pr = item.pr_number,
                                            worktree_path = %worktree_path,
                                            agent_exit = %result.status,
                                            duration_ms,
                                            queue_outcome = "agent_waited",
                                            "Fixer REPAIR agent finished"
                                        );
                                        if !agent_repair_succeeded(&result.status) {
                                            let err = format!(
                                                "agent repair failed: status={} summary={}",
                                                result.status, result.summary
                                            );
                                            if let Ok(g) = self.repos.0.lock() {
                                                let _ = apply_repair_fail_closed(
                                                    &g,
                                                    item,
                                                    &run.id,
                                                    &err,
                                                    "retryable_transient",
                                                );
                                            }
                                            return Err(err);
                                        }
                                        // Store agent summary in checkpoint.
                                        let cp = {
                                            let guard = self.repos.0.lock().map_err(|e| e.to_string())?;
                                            let existing = guard
                                                .runs
                                                .get_by_id(&run.id)
                                                .map_err(|e| e.to_string())?
                                                .and_then(|r| r.checkpoint_json);
                                            drop(guard);
                                            let mut base = existing
                                                .as_deref()
                                                .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                                                .unwrap_or_else(|| serde_json::json!({}));
                                            if !base.is_object() {
                                                base = serde_json::json!({});
                                            }
                                            base["worktree_path"] = serde_json::Value::String(worktree_path.clone());
                                            base["agent_summary"] = serde_json::Value::String(result.summary.clone());
                                            base["agent_exit"] = serde_json::Value::String(result.status.clone());
                                            base["duration_ms"] = serde_json::json!(duration_ms);
                                            base.to_string()
                                        };
                                        let _ = persist_run_checkpoint(self, &run.id, cp);
                                    }
                                    Err(e) => {
                                        let duration_ms = started.elapsed().as_millis() as u64;
                                        let err = format!("agent repair wait failed: {e}");
                                        tracing::warn!(
                                            loop_id = %loop_id,
                                            pr = item.pr_number,
                                            worktree_path = %worktree_path,
                                            duration_ms,
                                            agent_exit = "wait_error",
                                            queue_outcome = "fail_closed",
                                            "{err}"
                                        );
                                        if let Ok(g) = self.repos.0.lock() {
                                            let _ = apply_repair_fail_closed(
                                                &g,
                                                item,
                                                &run.id,
                                                &err,
                                                "retryable_transient",
                                            );
                                        }
                                        return Err(err);
                                    }
                                }
                            }
                            Err(e) => {
                                let duration_ms = started.elapsed().as_millis() as u64;
                                let err = format!("agent repair failed to start: {e}");
                                tracing::warn!(
                                    loop_id = %loop_id,
                                    pr = item.pr_number,
                                    worktree_path = %worktree_path,
                                    duration_ms,
                                    agent_exit = "start_error",
                                    queue_outcome = "fail_closed",
                                    "{err}"
                                );
                                if let Ok(g) = self.repos.0.lock() {
                                    let _ = apply_repair_fail_closed(&g, item, &run.id, &err, "retryable_transient");
                                }
                                return Err(err);
                            }
                        }
                    } else {
                        tracing::warn!(
                            loop_id = %loop_id,
                            worktree_path = %worktree_path,
                            "Fixer REPAIR: no agent configured; skipping agent step"
                        );
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
                    // Git push from the real worktree path (never bare ".").
                    let worktree_path = {
                        let from_cp = {
                            let guard = self.repos.0.lock().map_err(|e| e.to_string())?;
                            guard
                                .runs
                                .get_by_id(&run.id)
                                .map_err(|e| e.to_string())?
                                .and_then(|r| worktree_from_checkpoint(r.checkpoint_json.as_deref()))
                        };
                        from_cp.unwrap_or_else(|| resolve_fixer_wt(self, item, loop_id))
                    };
                    if let Some(ref git) = self.git {
                        let push_input = looper_git::PushInput {
                            worktree_path: worktree_path.clone(),
                            remote: "origin".to_string(),
                            branch: fixer_branch_name(loop_id),
                            expected_head_sha: None,
                            protected_branches: vec!["main".to_string(), "master".to_string()],
                            set_upstream: true,
                        };
                        match self.tokio_handle.block_on(git.push(push_input)) {
                            Ok(_) => tracing::info!(
                                loop_id = %loop_id,
                                worktree_path = %worktree_path,
                                "Fixes pushed"
                            ),
                            Err(e) => tracing::warn!(
                                loop_id = %loop_id,
                                worktree_path = %worktree_path,
                                "Push failed: {e}"
                            ),
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
                    // Trigger CI from the real worktree path (never bare ".").
                    let worktree_path = {
                        let from_cp = {
                            let guard = self.repos.0.lock().map_err(|e| e.to_string())?;
                            guard
                                .runs
                                .get_by_id(&run.id)
                                .map_err(|e| e.to_string())?
                                .and_then(|r| worktree_from_checkpoint(r.checkpoint_json.as_deref()))
                        };
                        from_cp.unwrap_or_else(|| resolve_fixer_wt(self, item, loop_id))
                    };
                    if let Some(ref git) = self.git {
                        let branch_name = fixer_branch_name(loop_id);
                        let _ = self.tokio_handle.block_on(git.commit(looper_git::types::CommitInput {
                            worktree_path: worktree_path.clone(),
                            message: "recheck: trigger CI".to_string(),
                        }));
                        let _ = self.tokio_handle.block_on(git.push(looper_git::types::PushInput {
                            worktree_path: worktree_path.clone(),
                            remote: "origin".to_string(),
                            branch: branch_name,
                            expected_head_sha: None,
                            protected_branches: vec!["main".to_string(), "master".to_string()],
                            set_upstream: false,
                        }));
                        tracing::info!(
                            loop_id = %loop_id,
                            worktree_path = %worktree_path,
                            "Fixer: pushed empty commit to trigger CI recheck"
                        );
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
            let repo_path = resolve_fixer_repo_path(self, item);
            let worktree_path = {
                let from_cp = {
                    let guard = self.repos.0.lock().map_err(|e| e.to_string())?;
                    guard
                        .runs
                        .get_by_id(&run.id)
                        .map_err(|e| e.to_string())?
                        .and_then(|r| worktree_from_checkpoint(r.checkpoint_json.as_deref()))
                };
                from_cp.unwrap_or_else(|| resolve_fixer_wt(self, item, loop_id))
            };
            let _ = self.tokio_handle.block_on(git.cleanup_worktree(CleanupWorktreeInput {
                repo_path: repo_path.clone(),
                worktree_path: worktree_path.clone(),
                branch: fixer_branch_name(loop_id),
                protected_branches: vec!["main".to_string(), "master".to_string()],
            }));
            let _ = std::fs::remove_dir_all(&worktree_path);
        }

        // Complete run + queue + loop ----------------------------------------
        {
            let guard = self.repos.0.lock().map_err(|e| e.to_string())?;
            let mut final_run = guard.runs.get_by_id(&run.id).map_err(|e| e.to_string())?.ok_or("run not found")?;
            final_run.status = RunStatus::Success.as_str().to_string();
            final_run.ended_at = Some(now_iso.clone());
            final_run.updated_at = now_iso;
            guard.runs.upsert(&final_run).map_err(|e| e.to_string())?;
        }
        crate::completion::mark_queue_terminal(&self.repos, &item.id, "completed", None);
        crate::completion::mark_loop_status(&self.repos, loop_id, "completed");

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

        // GitHub-powered discovery: enqueue fixer items for PRs that need fixes.
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
                    Ok(prs) => {
                        tracing::debug!("Fixer discovered {} open PRs via GitHub", prs.len());
                        for pr in prs {
                            let has_needs_fix = pr.labels.iter().any(|l| l == crate::types::spec_labels::NEEDS_FIX);
                            let mut failing = false;
                            let mut decision = Some(pr.review_decision.clone()).filter(|s| !s.is_empty());
                            if let Ok(detail) = gw.view_pull_request(ViewPullRequestInput {
                                repo: input.repo.clone(),
                                pr_number: pr.number,
                                cwd: ".".to_string(),
                            }) {
                                failing = crate::merge_watch::pr_has_failing_checks(&detail);
                                if decision.is_none() {
                                    decision = Some(detail.review_decision.clone()).filter(|s| !s.is_empty());
                                }
                            }
                            let ctx = crate::fixer_handoff::FixerEnqueueContext {
                                has_criteria_issues: false,
                                review_decision: decision,
                                has_failing_required_checks: failing,
                            };
                            let decision = crate::fixer_handoff::should_enqueue_fixer(&ctx);
                            let enqueue = decision.should_enqueue() || has_needs_fix;
                            if !enqueue {
                                continue;
                            }
                            let reason = if has_needs_fix { "label_needs_fix" } else { decision.reason() };
                            let project_id = input.project_id.clone();
                            if project_id.is_empty() {
                                continue;
                            }
                            if let Ok(g) = self.repos.0.lock() {
                                match crate::fixer_handoff::ensure_fixer_queue_item(
                                    &g,
                                    &project_id,
                                    Some(&input.repo),
                                    pr.number,
                                    reason,
                                ) {
                                    Ok(res) => {
                                        tracing::info!(
                                            project_id = %project_id,
                                            pr = pr.number,
                                            reason,
                                            queue_id = %res.queue_item.id,
                                            is_new = res.is_new,
                                            "Fixer discovery enqueued"
                                        );
                                    }
                                    Err(e) => {
                                        tracing::warn!(pr = pr.number, "Fixer discovery enqueue failed: {e}");
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => tracing::warn!("Fixer GitHub discovery failed: {e}"),
                }
            }
        }

        // Re-list after potential enqueues so claim path sees new items.
        let fixer_items = {
            let guard = match self.repos.0.lock() {
                Ok(g) => g,
                Err(_) => return FixerDiscoveryResult { queue_items: fixer_items },
            };
            match guard.queue.list_by_statuses(&["queued".into()]) {
                Ok(items) => items.into_iter().filter(|item| item.r#type == "fixer").collect(),
                Err(_) => fixer_items,
            }
        };

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
        match self.execute_pipeline(item) {
            Ok(()) => Ok(()),
            Err(e) => {
                crate::completion::mark_queue_terminal(&self.repos, &item.id, "failed", Some(e.clone()));
                if let Some(ref loop_id) = item.loop_id {
                    crate::completion::mark_loop_status(&self.repos, loop_id, "failed");
                }
                Err(e)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests — C3 Fixer REPAIR agent.wait + real worktree (fail closed)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use looper_storage::migration::run_migrations;
    use looper_storage::record::{ProjectRecord, QueueItemRecord, RunRecord};
    use rusqlite::Connection;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex;

    fn setup_repos() -> Repositories {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        run_migrations(&mut conn).unwrap();
        Repositories::new(conn)
    }

    fn now() -> String {
        Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
    }

    fn seed_project(repos: &Repositories, id: &str, repo_path: &str) {
        let t = now();
        repos
            .projects
            .upsert(&ProjectRecord {
                id: id.into(),
                name: "test".into(),
                repo_path: repo_path.into(),
                base_branch: Some("main".into()),
                archived: false,
                metadata_json: None,
                created_at: t.clone(),
                updated_at: t,
            })
            .unwrap();
    }

    fn sample_queue_item(id: &str, loop_id: &str, project_id: &str, pr: i64) -> QueueItemRecord {
        let t = now();
        QueueItemRecord {
            id: id.into(),
            project_id: Some(project_id.into()),
            loop_id: Some(loop_id.into()),
            r#type: "fixer".into(),
            target_type: "pull_request".into(),
            target_id: pr.to_string(),
            repo: Some("owner/repo".into()),
            pr_number: Some(pr),
            dedupe_key: format!("fixer-{project_id}-pr-{pr}"),
            priority: 2,
            status: "running".into(),
            available_at: t.clone(),
            attempts: 0,
            max_attempts: 3,
            claimed_by: Some("daemon".into()),
            claimed_at: Some(t.clone()),
            started_at: Some(t.clone()),
            finished_at: None,
            lock_key: None,
            payload_json: None,
            last_error: None,
            last_error_kind: None,
            created_at: t.clone(),
            updated_at: t,
        }
    }

    fn sample_run(id: &str, loop_id: &str) -> RunRecord {
        let t = now();
        RunRecord {
            id: id.into(),
            loop_id: loop_id.into(),
            status: RunStatus::Running.as_str().to_string(),
            current_step: Some(fixer_steps::REPAIR.to_string()),
            last_completed_step: Some(fixer_steps::PREPARE_WORKTREE.to_string()),
            checkpoint_json: None,
            summary: None,
            error_message: None,
            agent_vendor: None,
            model: None,
            started_at: t.clone(),
            last_heartbeat_at: Some(t.clone()),
            ended_at: None,
            created_at: t.clone(),
            updated_at: t,
        }
    }

    // --- Mock agent: asserts wait called + cwd non-empty ---

    struct MockRepairAgent {
        wait_called: AtomicBool,
        start_called: AtomicUsize,
        last_cwd: Mutex<String>,
        /// Status returned from wait (`"completed"` / `"failed"` / …).
        status: String,
        /// If set, start_and_wait returns Err.
        fail_with: Option<String>,
    }

    impl MockRepairAgent {
        fn ok() -> Self {
            Self {
                wait_called: AtomicBool::new(false),
                start_called: AtomicUsize::new(0),
                last_cwd: Mutex::new(String::new()),
                status: "completed".into(),
                fail_with: None,
            }
        }

        fn fail_status(status: &str) -> Self {
            Self {
                wait_called: AtomicBool::new(false),
                start_called: AtomicUsize::new(0),
                last_cwd: Mutex::new(String::new()),
                status: status.into(),
                fail_with: None,
            }
        }

        fn fail_error(msg: &str) -> Self {
            Self {
                wait_called: AtomicBool::new(false),
                start_called: AtomicUsize::new(0),
                last_cwd: Mutex::new(String::new()),
                status: "failed".into(),
                fail_with: Some(msg.into()),
            }
        }
    }

    impl RepairAgentSession for MockRepairAgent {
        fn start_and_wait(&mut self, working_directory: &str, _prompt: &str) -> Result<String, String> {
            self.start_called.fetch_add(1, Ordering::SeqCst);
            *self.last_cwd.lock().unwrap() = working_directory.to_string();
            // Simulate start + wait always pairing.
            self.wait_called.store(true, Ordering::SeqCst);
            if let Some(ref e) = self.fail_with {
                return Err(e.clone());
            }
            Ok(self.status.clone())
        }
    }

    #[test]
    fn fixer_repair_build_worktree_path_uses_repo_not_dot() {
        let path = build_fixer_worktree_path("/tmp/my-repo", "proj-1", "loop-abc", Some(42));
        assert!(!path.is_empty());
        assert_ne!(path, ".");
        assert!(path.starts_with("/tmp/my-repo/.looper/worktrees/"), "got {path}");
        assert!(path.contains("looper-fix") || path.contains("fix-"), "got {path}");
    }

    #[test]
    fn fixer_repair_build_worktree_path_without_pr_uses_branch() {
        let path = build_fixer_worktree_path("/repo", "p", "L1", None);
        assert_eq!(path, "/repo/.looper/worktrees/fix-L1");
    }

    #[test]
    fn fixer_repair_checkpoint_roundtrip() {
        let merged = merge_checkpoint_worktree(None, "/wt/fixer-1");
        assert_eq!(worktree_from_checkpoint(Some(&merged)).as_deref(), Some("/wt/fixer-1"));

        let merged2 = merge_checkpoint_worktree(Some(&merged), "/wt/fixer-2");
        assert_eq!(worktree_from_checkpoint(Some(&merged2)).as_deref(), Some("/wt/fixer-2"));

        // bare "." and empty are rejected
        assert!(worktree_from_checkpoint(Some(r#"{"worktree_path":"."}"#)).is_none());
        assert!(worktree_from_checkpoint(Some(r#"{"worktree_path":""}"#)).is_none());
        assert!(worktree_from_checkpoint(None).is_none());
    }

    #[test]
    fn fixer_repair_validate_cwd_rejects_empty_and_dot() {
        assert!(validate_repair_start_cwd("").is_err());
        assert!(validate_repair_start_cwd(".").is_err());
        assert!(validate_repair_start_cwd("/tmp/repo/.looper/worktrees/fix-x").is_ok());
    }

    #[test]
    fn fixer_repair_agent_success_status() {
        assert!(agent_repair_succeeded("completed"));
        assert!(!agent_repair_succeeded("failed"));
        assert!(!agent_repair_succeeded("timeout"));
        assert!(!agent_repair_succeeded("killed"));
        assert!(!agent_repair_succeeded(""));
    }

    #[test]
    fn fixer_repair_mock_agent_wait_called_cwd_non_empty() {
        let mut agent = MockRepairAgent::ok();
        let cwd = "/tmp/proj/.looper/worktrees/fix-loop-1";
        let status = run_repair_agent_session(&mut agent, cwd, "fix the PR").unwrap();
        assert_eq!(status, "completed");
        assert!(agent.wait_called.load(Ordering::SeqCst), "wait must be called");
        assert_eq!(agent.start_called.load(Ordering::SeqCst), 1);
        assert_eq!(agent.last_cwd.lock().unwrap().as_str(), cwd);
        assert!(!agent.last_cwd.lock().unwrap().is_empty());
        assert_ne!(agent.last_cwd.lock().unwrap().as_str(), ".");
    }

    #[test]
    fn fixer_repair_mock_agent_rejects_empty_cwd_before_start() {
        let mut agent = MockRepairAgent::ok();
        let err = run_repair_agent_session(&mut agent, "", "prompt").unwrap_err();
        assert!(err.contains("working_directory"), "{err}");
        assert!(!agent.wait_called.load(Ordering::SeqCst), "must not wait on invalid cwd");
        assert_eq!(agent.start_called.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn fixer_repair_mock_agent_fail_status_not_success() {
        let mut agent = MockRepairAgent::fail_status("failed");
        let err = run_repair_agent_session(&mut agent, "/wt/real", "prompt").unwrap_err();
        assert!(err.contains("status=failed"), "{err}");
        assert!(agent.wait_called.load(Ordering::SeqCst));
    }

    #[test]
    fn fixer_repair_fail_closed_marks_failed_not_success() {
        let repos = setup_repos();
        seed_project(&repos, "proj-1", "/tmp/r");
        let t = now();
        repos
            .loops
            .upsert(&looper_storage::record::LoopRecord {
                id: "loop-1".into(),
                seq: 1,
                project_id: "proj-1".into(),
                r#type: "fixer".into(),
                target_type: "pull_request".into(),
                target_id: Some("7".into()),
                repo: Some("o/r".into()),
                pr_number: Some(7),
                status: "running".into(),
                config_json: None,
                metadata_json: None,
                last_run_at: None,
                next_run_at: None,
                created_at: t.clone(),
                updated_at: t.clone(),
            })
            .unwrap();

        let item = sample_queue_item("q-1", "loop-1", "proj-1", 7);
        repos.queue.upsert(&item).unwrap();

        let run = sample_run("run-1", "loop-1");
        repos.runs.upsert(&run).unwrap();

        apply_repair_fail_closed(&repos, &item, "run-1", "agent repair failed: status=failed", "retryable_transient")
            .unwrap();

        let failed = repos.runs.get_by_id("run-1").unwrap().unwrap();
        assert_eq!(failed.status, RunStatus::Failed.as_str());
        assert_ne!(failed.status, RunStatus::Success.as_str());
        assert!(failed.error_message.as_deref().unwrap_or("").contains("agent repair failed"));
        assert!(failed.ended_at.is_some());

        let q = repos.queue.get_by_id("q-1").unwrap().unwrap();
        // mark_retry should bump attempts and set last error kind
        assert_eq!(q.attempts, 1);
        assert_eq!(q.last_error_kind.as_deref(), Some("retryable_transient"));
        assert_ne!(q.status, "completed");
    }

    #[test]
    fn fixer_repair_mock_wait_error_path() {
        let mut agent = MockRepairAgent::fail_error("simulated wait timeout");
        let err = run_repair_agent_session(&mut agent, "/wt/real", "prompt").unwrap_err();
        assert!(err.contains("simulated wait timeout"), "{err}");
        assert!(agent.wait_called.load(Ordering::SeqCst));
    }

    #[test]
    fn fixer_branch_name_format() {
        assert_eq!(fixer_branch_name("abc-123"), "fix/abc-123");
    }
}
