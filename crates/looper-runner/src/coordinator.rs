//! Coordinator: tick-based discovery and scheduling orchestrator.
//!
//! The Coordinator is the top-level runner that fires on every scheduler tick
//! to discover projects and their PRs, classify them via MergeWatch, and
//! enqueue appropriate queue items.
//!
//! It implements [`CoordinatorScheduler`].

use std::sync::Arc;

use looper_github::types::ListOpenPullRequestsInput;
use looper_scheduler::scheduler::SendRepos;
use looper_scheduler::types::{
    Context, CoordinatorDiscoveryInput, CoordinatorDiscoveryResult,
    CoordinatorScheduler, SchedulerConfig,
};
use looper_storage::record::QueueItemRecord;

use crate::types::{DispatchConfig, WatchAction, WatchActionKind};

/// The coordinator runner, registered in `HandlerMap::coordinator`.
pub struct Coordinator {
    pub config: SchedulerConfig,
    pub repos: Arc<SendRepos>,
    pub github: Option<Arc<looper_github::Gateway>>,
    pub tokio_handle: tokio::runtime::Handle,
}

// SAFETY: SendRepos is Send+Sync (Mutex<Repositories>). Gateway is Send+Sync.
unsafe impl Send for Coordinator {}
unsafe impl Sync for Coordinator {}

impl Coordinator {
    pub fn new(
        config: &SchedulerConfig,
        repos: Arc<SendRepos>,
        github: Option<Arc<looper_github::Gateway>>,
        tokio_handle: tokio::runtime::Handle,
    ) -> Self {
        Self {
            config: config.clone(),
            repos,
            github,
            tokio_handle,
        }
    }
}

impl CoordinatorScheduler for Coordinator {
    fn discover_issues(
        &self,
        ctx: &Context,
        input: CoordinatorDiscoveryInput,
    ) -> CoordinatorDiscoveryResult {
        tracing::debug!(
            "Coordinator discover_issues — project={}, repo={}",
            input.project_id,
            input.repo
        );

        if ctx.is_cancelled() {
            return CoordinatorDiscoveryResult::default();
        }

        // The coordinator's job is to:
        //   1. Query open PRs for this project (via repos or GitHub gateway).
        //   2. Run classify_pr (from merge_watch) on each.
        //   3. Map the resulting WatchActionKind to queue dispatch types.
        //   4. Enqueue QueueItemRecords for the scheduler to claim.

        let mut queue_items: Vec<QueueItemRecord> = Vec::new();

        let guard = match self.repos.0.lock() {
            Ok(g) => g,
            Err(e) => {
                tracing::error!("Coordinator repo lock: {e}");
                return CoordinatorDiscoveryResult::default();
            }
        };

        // Fetch all loops for this project.
        let all_loops = match guard.loops.list() {
            Ok(l) => l,
            Err(e) => {
                tracing::error!("Coordinator list loops: {e}");
                return CoordinatorDiscoveryResult::default();
            }
        };

        let project_loops: Vec<_> = all_loops
            .into_iter()
            .filter(|l| {
                l.project_id == input.project_id
                    && l.pr_number.is_some()
                    && !is_terminal_loop_status(&l.status)
            })
            .collect();

        for l in &project_loops {
            if ctx.is_cancelled() {
                break;
            }

            // Check if there's already a pending queue item for this loop
            // to avoid duplicates.
            let pending = match guard.queue.list() {
                Ok(items) => items.iter().any(|q| {
                    q.loop_id.as_deref() == Some(&l.id)
                        && (q.status == "queued" || q.status == "running")
                }),
                Err(_) => false,
            };

            if !pending {
                // Determine the runner type based on the loop type.
                let queue_type = match l.r#type.as_str() {
                    "review" | "fixer" => l.r#type.clone(),
                    _ => "reviewer".to_string(),
                };

                let now_iso = chrono::Utc::now()
                    .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                    .to_string();

                let item = QueueItemRecord {
                    id: format!("coord-{}-{}", l.id, queue_type),
                    project_id: Some(input.project_id.clone()),
                    loop_id: Some(l.id.clone()),
                    r#type: queue_type.clone(),
                    target_type: String::new(),
                    target_id: String::new(),
                    dedupe_key: format!("{}/{}", l.id, queue_type),
                    priority: 2,
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
                    repo: l.repo.clone(),
                    pr_number: l.pr_number,
                    created_at: now_iso.clone(),
                    updated_at: now_iso,
                };

                tracing::info!(
                    "Coordinator enqueued {} item for loop {}",
                    queue_type,
                    l.id
                );
                queue_items.push(item);
            }
        }

        // --- GitHub-powered PR discovery ---
        if let Some(ref gw) = self.github {
            let repo = &input.repo;
            if !repo.is_empty() {
                match gw.list_open_pull_requests(ListOpenPullRequestsInput {
                    repo: repo.clone(),
                    cwd: ".".to_string(),
                    limit: 50,
                    label: String::new(),
                    labels: vec![],
                    author: String::new(),
                    base_ref_name: String::new(),
                    timeout: None,
                }) {
                    Ok(prs) => {
                        for pr in &prs {
                            if ctx.is_cancelled() {
                                break;
                            }
                            let now_iso = chrono::Utc::now()
                                .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                                .to_string();
                            let item = QueueItemRecord {
                                id: format!("github-review-{}", pr.number),
                                project_id: Some(input.project_id.clone()),
                                loop_id: None,
                                r#type: "reviewer".into(),
                                target_type: "pr".into(),
                                target_id: pr.number.to_string(),
                                dedupe_key: format!("pr-review-{}", pr.number),
                                priority: 2,
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
                                repo: Some(repo.clone()),
                                pr_number: Some(pr.number),
                                created_at: now_iso.clone(),
                                updated_at: now_iso,
                            };
                            queue_items.push(item);
                        }
                        tracing::debug!(
                            "Coordinator GitHub PR discovery — {} open PRs for {}",
                            prs.len(),
                            repo
                        );
                    }
                    Err(e) => {
                        tracing::warn!("Coordinator GitHub PR discovery failed: {e}");
                    }
                }
            }
        }

        tracing::debug!(
            "Coordinator enqueued {} items for project {}",
            queue_items.len(),
            input.project_id
        );

        CoordinatorDiscoveryResult { queue_items }
    }
}

fn is_terminal_loop_status(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "cancelled" | "terminated")
}

/// Main coordinator tick — called from the scheduler's coordinator
/// integration point (if any).
pub async fn execute_coordinator_tick(
    _handler_map: &looper_scheduler::types::HandlerMap,
) -> Result<(), String> {
    // This function is kept as an integration hook for future scheduler
    // versions that call the coordinator tick asynchronously.  Currently
    // the coordinator logic lives in `discover_issues` above.
    tracing::debug!("Coordinator tick: no-op (logic runs via discover_issues)");
    Ok(())
}

/// Enqueue a queue item from a watch action.
///
/// Maps each [`WatchActionKind`] to the appropriate queue dispatch type.
pub fn action_to_queue_dispatch(
    action: &WatchAction,
    _dispatch_config: &DispatchConfig,
) -> Option<String> {
    match action.kind {
        WatchActionKind::MarkMerged => {
            Some(format!("merge-snapshot: PR #{} ({})", action.pr_number, action.pr_title))
        }
        WatchActionKind::ClosePR => {
            Some(format!("close-pr: PR #{} ({})", action.pr_number, action.pr_title))
        }
        WatchActionKind::ReengageReview => {
            Some(format!("reengage-review: PR #{} ({})", action.pr_number, action.pr_title))
        }
        WatchActionKind::MergeReady => {
            Some(format!("merge-ready: PR #{} ({})", action.pr_number, action.pr_title))
        }
        WatchActionKind::RetryCheck => {
            Some(format!("retry-check: PR #{} ({})", action.pr_number, action.pr_title))
        }
        WatchActionKind::Stuck => {
            Some(format!("stuck: PR #{} ({})", action.pr_number, action.pr_title))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_action_to_queue_dispatch_merge_ready() {
        let action = WatchAction {
            kind: WatchActionKind::MergeReady,
            pr_number: 42,
            pr_title: "feat: widget".into(),
            snapshot: None,
            first_unknown_at: None,
            deadline_exceeded: false,
            retries_left: 0,
            suggested_delay_secs: 0,
            exhausted: false,
        };
        let dispatch = action_to_queue_dispatch(&action, &DispatchConfig::default());
        assert!(dispatch.is_some());
        assert!(dispatch.unwrap().contains("merge-ready"));
    }

    #[test]
    fn test_action_to_queue_dispatch_stuck() {
        let action = WatchAction {
            kind: WatchActionKind::Stuck,
            pr_number: 99,
            pr_title: "fix: critical bug".into(),
            snapshot: None,
            first_unknown_at: None,
            deadline_exceeded: true,
            retries_left: 0,
            suggested_delay_secs: 600,
            exhausted: true,
        };
        let dispatch = action_to_queue_dispatch(&action, &DispatchConfig::default());
        assert!(dispatch.is_some());
        assert!(dispatch.unwrap().contains("stuck"));
    }

    #[test]
    fn test_action_to_queue_dispatch_ignored() {
        let action = WatchAction {
            kind: WatchActionKind::Merged,
            pr_number: 1,
            pr_title: "done".into(),
            snapshot: None,
            first_unknown_at: None,
            deadline_exceeded: false,
            retries_left: 0,
            suggested_delay_secs: 0,
            exhausted: false,
        };
        // Merged (already closed) is not a coordinator action
        assert!(action_to_queue_dispatch(&action, &DispatchConfig::default()).is_none());
    }

    #[test]
    fn test_is_terminal_loop_status() {
        assert!(is_terminal_loop_status("completed"));
        assert!(is_terminal_loop_status("failed"));
        assert!(is_terminal_loop_status("cancelled"));
        assert!(is_terminal_loop_status("terminated"));
        assert!(!is_terminal_loop_status("running"));
        assert!(!is_terminal_loop_status("queued"));
        assert!(!is_terminal_loop_status("paused"));
    }
}
