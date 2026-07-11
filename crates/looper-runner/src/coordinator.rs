//! Coordinator: tick-based discovery and scheduling orchestrator.
//!
//! The Coordinator is the top-level runner that fires on every scheduler tick
//! to discover projects and their PRs, classify them via MergeWatch, and
//! enqueue appropriate queue items.
//!
//! It implements [`CoordinatorScheduler`] and runs its logic through a
//! middleware chain for composability: quality gate → discovery → outcome
//! recording → patrol monitoring.

use std::sync::Arc;

use looper_github::types::{
    ClosePullRequestInput, IssueLabelsInput, ListOpenIssuesInput, ListOpenPullRequestsInput, ViewIssueInput,
    ViewPullRequestInput,
};
use looper_scheduler::scheduler::SendRepos;
use looper_scheduler::types::{
    Context, CoordinatorDiscoveryInput, CoordinatorDiscoveryResult, CoordinatorScheduler, SchedulerConfig,
};
use looper_storage::record::QueueItemRecord;
use std::process::Command;

use crate::types::{WatchAction, WatchActionKind};
use looper_scheduler::types::DispatchConfig;

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
        Self { config: config.clone(), repos, github, tokio_handle }
    }
}

impl CoordinatorScheduler for Coordinator {
    fn discover_issues(&self, ctx: &Context, input: CoordinatorDiscoveryInput) -> CoordinatorDiscoveryResult {
        tracing::debug!("Coordinator discover_issues — project={}, repo={}", input.project_id, input.repo);

        if ctx.is_cancelled() {
            return CoordinatorDiscoveryResult::default();
        }

        // The coordinator's job is to:
        //   1. Query open PRs for this project (via repos or GitHub gateway).
        //   2. Run classify_pr (from merge_watch) on each.
        //   3. Map the resulting WatchActionKind to queue dispatch types.
        //   4. Enqueue QueueItemRecords for the scheduler to claim.
        //
        // After building the initial queue items, the middleware pipeline
        // applies: quality gate → outcome recording → patrol monitoring.

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
                l.project_id == input.project_id && l.pr_number.is_some() && !is_terminal_loop_status(&l.status)
            })
            .collect();

        // Cache PR open status to avoid repeated API calls
        let mut pr_open_cache: std::collections::HashMap<i64, bool> = std::collections::HashMap::new();
        for l in &project_loops {
            if ctx.is_cancelled() {
                break;
            }

            // If this loop references a PR, check if it's still open
            if let Some(pr_num) = l.pr_number {
                if let std::collections::hash_map::Entry::Vacant(e) = pr_open_cache.entry(pr_num) {
                    let is_open = if let Some(ref gw) = self.github {
                        // Use the GitHub gateway to check if PR is open
                        let pr_check = gw.view_pull_request(looper_github::types::ViewPullRequestInput {
                            repo: input.repo.clone(),
                            pr_number: pr_num,
                            cwd: ".".to_string(),
                        });
                        match pr_check {
                            Ok(pr) => pr.state == "OPEN",
                            Err(_) => false,
                        }
                    } else {
                        true // assume open if no github
                    };
                    e.insert(is_open);
                }
                if !pr_open_cache.get(&pr_num).copied().unwrap_or(false) {
                    // PR is closed - skip this loop
                    continue;
                }
            }

            // Check if there's already a pending queue item for this PR
            // across ALL loops to avoid duplicate workers/reviewers.
            let pending = match guard.queue.list() {
                Ok(items) => items.iter().any(|q| {
                    // Match by pr_number if available, otherwise fall back to loop_id
                    let pr_match = match (q.pr_number, l.pr_number) {
                        (Some(qpr), Some(lpr)) => qpr == lpr,
                        _ => q.loop_id.as_deref() == Some(&l.id),
                    };
                    pr_match && (q.status == "queued" || q.status == "running")
                }),
                Err(_) => false,
            };

            // If PR was checked and is not open, cancel the loop
            if let Some(pr_num) = l.pr_number {
                if let Some(false) = pr_open_cache.get(&pr_num) {
                    let mut updated = l.clone();
                    updated.status = "cancelled".to_string();
                    updated.updated_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
                    let _ = guard.loops.upsert(&updated);
                    tracing::info!("Coordinator: cancelled loop {} for closed PR #{}", l.id, pr_num);
                    continue;
                }
            }

            if !pending {
                // Determine the runner type based on the loop type.
                let queue_type = match l.r#type.as_str() {
                    "review" | "fixer" => l.r#type.clone(),
                    _ => "reviewer".to_string(),
                };

                let now_iso = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();

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

                tracing::info!("Coordinator enqueued {} item for loop {}", queue_type, l.id);
                queue_items.push(item);
            }
        }

        // --- Spec-ready → auto-transition to dispatch/implement ---
        // When a spec PR is marked looper:spec-ready, the linked issue should
        // transition from dispatch/plan to dispatch/implement so the worker
        // picks it up for implementation.
        if let Some(ref gw) = self.github {
            let repo = &input.repo;
            if !repo.is_empty() {
                // Find PRs with looper:spec-ready label (spec-review approved)
                if let Ok(prs) = gw.list_open_pull_requests(ListOpenPullRequestsInput {
                    repo: repo.clone(),
                    cwd: ".".to_string(),
                    limit: 50,
                    label: "looper:spec-ready".to_string(),
                    labels: vec![],
                    author: String::new(),
                    base_ref_name: String::new(),
                    timeout: None,
                }) {
                    for pr_summary in &prs {
                        // View the PR to get the body which contains the linked issue
                        if let Ok(pr_detail) = gw.view_pull_request(ViewPullRequestInput {
                            repo: repo.clone(),
                            pr_number: pr_summary.number,
                            cwd: ".".to_string(),
                        }) {
                            // Find linked issue number from PR title ([N] title) or body
                            let issue_num = pr_detail
                                .title
                                .strip_prefix('[')
                                .and_then(|rest| rest.split(']').next())
                                .and_then(|n| n.parse::<i64>().ok())
                                .or_else(|| {
                                    pr_detail.body.lines().find_map(|line| {
                                        let line = line.trim();
                                        if line.starts_with("##") {
                                            return None;
                                        }
                                        // Look for "issue #N" anywhere in the line
                                        if let Some(pos) = line.find("issue #") {
                                            let after = &line[pos + 7..];
                                            after.split_whitespace().next().and_then(|s| {
                                                s.trim_end_matches(|c: char| !c.is_ascii_digit()).parse::<i64>().ok()
                                            })
                                        } else {
                                            None
                                        }
                                    })
                                });
                            if let Some(linked_issue) = issue_num {
                                if let Ok(issue) = gw.view_issue(ViewIssueInput {
                                    repo: repo.clone(),
                                    issue_number: linked_issue,
                                    cwd: ".".to_string(),
                                }) {
                                    let has_dispatch_plan =
                                        issue.labels.iter().any(|l| l == "dispatch/plan" || l == "looper:plan");
                                    let has_dispatch_implement = issue.labels.iter().any(|l| {
                                        l == "dispatch/implement"
                                            || l == "looper:worker-ready"
                                            || l == "looper:implement"
                                    });
                                    if has_dispatch_plan && !has_dispatch_implement {
                                        tracing::info!(
                                            "Spec transition: PR #{} spec-ready → issue #{} dispatch/plan → dispatch/implement",
                                            pr_summary.number, linked_issue
                                        );
                                        let _ = gw.remove_issue_labels(IssueLabelsInput {
                                            repo: repo.clone(),
                                            issue_number: linked_issue,
                                            labels: vec!["dispatch/plan".into()],
                                            cwd: ".".to_string(),
                                        });
                                        let _ = gw.add_issue_labels(IssueLabelsInput {
                                            repo: repo.clone(),
                                            issue_number: linked_issue,
                                            labels: vec!["dispatch/implement".into()],
                                            cwd: ".".to_string(),
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // --- Spec-review timeout: auto-close stale spec-reviewing PRs >7 days ---
        // If a spec PR has been in looper:spec-reviewing for more than 7 days with no
        // updates, auto-close it with a comment explaining the timeout. The human can
        // re-open if they still want the change — this prevents spec PR debt from
        // accumulating in the open-PR list.
        if let Some(ref gw) = self.github {
            let repo = &input.repo;
            if !repo.is_empty() {
                const SPEC_TIMEOUT_DAYS: f64 = 7.0;
                if let Ok(prs) = gw.list_open_pull_requests(ListOpenPullRequestsInput {
                    repo: repo.clone(),
                    cwd: ".".to_string(),
                    limit: 50,
                    label: "looper:spec-reviewing".to_string(),
                    labels: vec![],
                    author: String::new(),
                    base_ref_name: String::new(),
                    timeout: None,
                }) {
                    for pr_summary in &prs {
                        // Parse the PR's updated_at to see how long it's been stale
                        let updated_at = &pr_summary.updated_at;
                        if let Ok(updated_dt) = chrono::DateTime::parse_from_rfc3339(updated_at) {
                            let elapsed = chrono::Utc::now().signed_duration_since(updated_dt);
                            if elapsed.num_hours() as f64 >= SPEC_TIMEOUT_DAYS * 24.0 {
                                tracing::info!(
                                    "Coordinator: spec-review timeout — PR #{} stale for {:.1}h, auto-closing",
                                    pr_summary.number,
                                    elapsed.num_hours() as f64
                                );
                                // Post a comment explaining the closure
                                let _ = gw.create_issue_comment(looper_github::types::IssueCommentInput {
                                    repo: repo.clone(),
                                    issue_number: pr_summary.number,
                                    body: format!(
                                        "Auto-closing this spec PR because it has been in `looper:spec-reviewing` \
                                             for more than {:.0} days without activity. \
                                             The linked issue will remain open. Re-open if this spec still applies.",
                                        SPEC_TIMEOUT_DAYS
                                    ),
                                    cwd: ".".to_string(),
                                });
                                // Remove the spec-reviewing label
                                let _ = gw.remove_issue_labels(IssueLabelsInput {
                                    repo: repo.clone(),
                                    issue_number: pr_summary.number,
                                    labels: vec!["looper:spec-reviewing".into()],
                                    cwd: ".".to_string(),
                                });
                                // Close the PR
                                let _ = gw.close_pull_request(ClosePullRequestInput {
                                    repo: repo.clone(),
                                    pr_number: pr_summary.number,
                                    cwd: ".".to_string(),
                                });
                            }
                        }
                    }
                }
            }
        }

        // --- Dispatch phase: auto-trigger looper:plan/looper:implement from dispatch/* ---
        if let Some(ref gw) = self.github {
            let repo = &input.repo;
            if !repo.is_empty() {
                // Dispatch/plan → add looper:plan
                if let Ok(issues) = gw.list_open_issues(ListOpenIssuesInput {
                    repo: repo.clone(),
                    cwd: ".".to_string(),
                    limit: 50,
                    assignee: String::new(),
                    label: "dispatch/plan".to_string(),
                    labels: vec![],
                }) {
                    for issue in &issues {
                        if let Ok(detail) = gw.view_issue(ViewIssueInput {
                            repo: repo.clone(),
                            issue_number: issue.number,
                            cwd: ".".to_string(),
                        }) {
                            // Permission check: only allow authorized users
                            if !crate::permissions::user_authorized_for_dispatch(
                                &detail.author,
                                repo,
                                &self.config.dispatch_config,
                                gw,
                            ) {
                                tracing::info!(
                                    "Dispatch: issue #{} author '{}' not authorized for dispatch/plan, skipping",
                                    issue.number,
                                    detail.author
                                );
                                continue;
                            }

                            let has_plan = detail.labels.iter().any(|l| l == "looper:plan");
                            if !has_plan {
                                tracing::info!(
                                    "Dispatch: issue #{} has dispatch/plan, adding looper:plan",
                                    issue.number
                                );
                                let _ = gw.add_issue_labels(IssueLabelsInput {
                                    repo: repo.clone(),
                                    issue_number: issue.number,
                                    labels: vec!["looper:plan".into()],
                                    cwd: ".".to_string(),
                                });
                            }
                        }
                    }
                }
                // Dispatch/implement → add looper:worker-ready (Go worker discovery label)
                if let Ok(issues) = gw.list_open_issues(ListOpenIssuesInput {
                    repo: repo.clone(),
                    cwd: ".".to_string(),
                    limit: 50,
                    assignee: String::new(),
                    label: "dispatch/implement".to_string(),
                    labels: vec![],
                }) {
                    for issue in &issues {
                        if let Ok(detail) = gw.view_issue(ViewIssueInput {
                            repo: repo.clone(),
                            issue_number: issue.number,
                            cwd: ".".to_string(),
                        }) {
                            // Permission check: only allow authorized users
                            if !crate::permissions::user_authorized_for_dispatch(
                                &detail.author,
                                repo,
                                &self.config.dispatch_config,
                                gw,
                            ) {
                                tracing::info!(
                                    "Dispatch: issue #{} author '{}' not authorized for dispatch/implement, skipping",
                                    issue.number,
                                    detail.author
                                );
                                continue;
                            }

                            let has_worker_ready =
                                detail.labels.iter().any(|l| l == "looper:worker-ready" || l == "looper:implement");
                            if !has_worker_ready {
                                tracing::info!(
                                    "Dispatch: issue #{} has dispatch/implement, adding looper:worker-ready",
                                    issue.number
                                );
                                let _ = gw.add_issue_labels(IssueLabelsInput {
                                    repo: repo.clone(),
                                    issue_number: issue.number,
                                    labels: vec!["looper:worker-ready".into()],
                                    cwd: ".".to_string(),
                                });
                                // Remove looper:plan so the planner doesn't re-discover it
                                let _ = gw.remove_issue_labels(IssueLabelsInput {
                                    repo: repo.clone(),
                                    issue_number: issue.number,
                                    labels: vec!["looper:plan".into()],
                                    cwd: ".".to_string(),
                                });
                            }
                        }
                    }
                }
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
                        for pr_summary in &prs {
                            if ctx.is_cancelled() {
                                break;
                            }

                            // Fetch full PR detail for merge-watch classification
                            let mut needs_fixer = false;
                            if let Ok(pr_detail) = gw.view_pull_request(ViewPullRequestInput {
                                repo: repo.clone(),
                                pr_number: pr_summary.number,
                                cwd: ".".to_string(),
                            }) {
                                let action = crate::merge_watch::classify_pr(&pr_detail, None, None);
                                if let Some(ref action) = action {
                                    match action.kind {
                                        crate::types::WatchActionKind::RedCI
                                        | crate::types::WatchActionKind::Conflict
                                        | crate::types::WatchActionKind::ReengageReview => {
                                            needs_fixer = true;
                                            tracing::info!(
                                                "Coordinator merged-watch: PR #{} → {:?}",
                                                pr_summary.number,
                                                action.kind
                                            );
                                        }
                                        crate::types::WatchActionKind::MergeReady => {
                                            // Execute auto-merge on merge-ready PRs
                                            tracing::info!(
                                                "Coordinator: PR #{} is merge-ready, executing auto-merge",
                                                pr_summary.number
                                            );
                                            match execute_auto_merge(pr_summary.number, repo) {
                                                Ok(()) => tracing::info!(
                                                    "Auto-merge enabled for PR #{} in {}",
                                                    pr_summary.number,
                                                    repo
                                                ),
                                                Err(e) => tracing::warn!(
                                                    "Auto-merge failed for PR #{} in {}: {}",
                                                    pr_summary.number,
                                                    repo,
                                                    e
                                                ),
                                            }
                                            continue;
                                        }
                                        crate::types::WatchActionKind::MarkMerged
                                        | crate::types::WatchActionKind::ClosePR => {
                                            continue;
                                        }
                                        _ => {}
                                    }
                                }
                            }

                            if needs_fixer {
                                let now_iso = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
                                let fix_item = QueueItemRecord {
                                    id: format!("github-fix-{}", pr_summary.number),
                                    project_id: Some(input.project_id.clone()),
                                    loop_id: None,
                                    r#type: "fixer".into(),
                                    target_type: "pr".into(),
                                    target_id: pr_summary.number.to_string(),
                                    dedupe_key: format!("pr-fix-{}", pr_summary.number),
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
                                    pr_number: Some(pr_summary.number),
                                    created_at: now_iso.clone(),
                                    updated_at: now_iso,
                                };
                                queue_items.push(fix_item);
                                continue;
                            }

                            // Default: enqueue a reviewer item for this PR
                            let now_iso = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
                            let item = QueueItemRecord {
                                id: format!("github-review-{}", pr_summary.number),
                                project_id: Some(input.project_id.clone()),
                                loop_id: None,
                                r#type: "reviewer".into(),
                                target_type: "pr".into(),
                                target_id: pr_summary.number.to_string(),
                                dedupe_key: format!("pr-review-{}", pr_summary.number),
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
                                pr_number: Some(pr_summary.number),
                                created_at: now_iso.clone(),
                                updated_at: now_iso,
                            };
                            queue_items.push(item);
                        }
                        tracing::debug!("Coordinator GitHub PR discovery — {} open PRs for {}", prs.len(), repo);
                    }
                    Err(e) => {
                        tracing::warn!("Coordinator GitHub PR discovery failed: {e}");
                    }
                }
            }
        }

        tracing::debug!("Coordinator enqueued {} items for project {}", queue_items.len(), input.project_id);

        CoordinatorDiscoveryResult { queue_items }
    }
}

fn is_terminal_loop_status(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "cancelled" | "terminated")
}

/// Main coordinator tick — called from the scheduler's coordinator
/// integration point (if any).
pub async fn execute_coordinator_tick(_handler_map: &looper_scheduler::types::HandlerMap) -> Result<(), String> {
    // This function is kept as an integration hook for future scheduler
    // versions that call the coordinator tick asynchronously.  Currently
    // the coordinator logic lives in `discover_issues` above.
    tracing::debug!("Coordinator tick: no-op (logic runs via discover_issues)");
    Ok(())
}

/// Enqueue a queue item from a watch action.
///
/// Maps each [`WatchActionKind`] to the appropriate queue dispatch type.
pub fn action_to_queue_dispatch(action: &WatchAction, _dispatch_config: &DispatchConfig) -> Option<String> {
    match action.kind {
        WatchActionKind::MarkMerged => Some(format!("merge-snapshot: PR #{} ({})", action.pr_number, action.pr_title)),
        WatchActionKind::ClosePR => Some(format!("close-pr: PR #{} ({})", action.pr_number, action.pr_title)),
        WatchActionKind::ReengageReview => {
            Some(format!("reengage-review: PR #{} ({})", action.pr_number, action.pr_title))
        }
        WatchActionKind::MergeReady => Some(format!("merge-ready: PR #{} ({})", action.pr_number, action.pr_title)),
        WatchActionKind::RetryCheck => Some(format!("retry-check: PR #{} ({})", action.pr_number, action.pr_title)),
        WatchActionKind::Stuck => Some(format!("stuck: PR #{} ({})", action.pr_number, action.pr_title)),
        _ => None,
    }
}

/// Execute auto-merge on a pull request using the `gh pr merge` command.
///
/// Runs `gh pr merge <num> --auto --squash -R <repo>` to enable GitHub's
/// auto-merge (merge-when-checks-pass) functionality with squash strategy.
#[allow(clippy::disallowed_methods)]
pub fn execute_auto_merge(pr_number: i64, repo: &str) -> Result<(), String> {
    let output = Command::new("gh")
        .args(["pr", "merge", &pr_number.to_string(), "--auto", "--squash", "-R", repo])
        .output()
        .map_err(|e| format!("failed to execute gh pr merge: {e}"))?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        tracing::debug!("gh pr merge output: {stdout}");
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("gh pr merge failed: {stderr}"))
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
