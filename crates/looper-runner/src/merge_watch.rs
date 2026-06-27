//! MergeWatch: PR auto-merge state classifier.
//!
//! Determines, on every coordinator tick, whether a tracked pull request has been
//! merged, needs another review round, or is stuck.  The classifier emits
//! [`WatchAction`] values that the coordinator feeds into the queue system.

use crate::types::{PriorWatchMarker, RetryBudget, ReviewDecision, WatchAction, WatchActionKind};
use looper_github::types::PullRequestDetail;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Classify a single pull request and produce the next action, if any.
///
/// Returns `None` when no action is needed (PR is still in progress or already
/// handled).
pub fn classify_pr(
    pr: &PullRequestDetail,
    prior: Option<&PriorWatchMarker>,
    budget: Option<&RetryBudget>,
) -> Option<WatchAction> {
    // 1. Merged (state=closed + merged_at non-empty) → clean up.
    if pr.state == "closed" && !pr.merged_at.is_empty() {
        return Some(WatchAction {
            kind: WatchActionKind::MarkMerged,
            pr_number: pr.number,
            pr_title: pr.title.clone(),
            snapshot: None,
            first_unknown_at: None,
            deadline_exceeded: false,
            retries_left: 0,
            suggested_delay_secs: 0,
            exhausted: false,
        });
    }

    // 2. Closed without merge → close.
    if pr.state == "closed" {
        return Some(WatchAction {
            kind: WatchActionKind::ClosePR,
            pr_number: pr.number,
            pr_title: pr.title.clone(),
            snapshot: None,
            first_unknown_at: None,
            deadline_exceeded: false,
            retries_left: 0,
            suggested_delay_secs: 0,
            exhausted: false,
        });
    }

    // 3. Parse review decision from the string field.
    let decision = ReviewDecision::from_github_string(&pr.review_decision);

    // 4. Changes requested (review decision) → re-engage.
    if decision == Some(ReviewDecision::ChangesRequested) {
        return Some(WatchAction {
            kind: WatchActionKind::ReengageReview,
            pr_number: pr.number,
            pr_title: pr.title.clone(),
            snapshot: None,
            first_unknown_at: None,
            deadline_exceeded: false,
            retries_left: 0,
            suggested_delay_secs: 0,
            exhausted: false,
        });
    }

    // 5. Mergeable, approved, and no pending checks → merge ready.
    if pr.mergeable == Some(true) && decision == Some(ReviewDecision::Approved) {
        // Check CI status before declaring merge-ready
        if pr_has_failing_checks(pr) {
            let failed_checks = pr_failing_check_names(pr);
            return Some(WatchAction {
                kind: WatchActionKind::RedCI,
                pr_number: pr.number,
                pr_title: format!("{} (failing checks: {})", pr.title, failed_checks.join(", ")),
                snapshot: None,
                first_unknown_at: None,
                deadline_exceeded: false,
                retries_left: 0,
                suggested_delay_secs: 0,
                exhausted: false,
            });
        }
        if pr_has_pending_checks(pr) {
            // Checks still running — wait
            return None;
        }
        return Some(WatchAction {
            kind: WatchActionKind::MergeReady,
            pr_number: pr.number,
            pr_title: pr.title.clone(),
            snapshot: None,
            first_unknown_at: None,
            deadline_exceeded: false,
            retries_left: 0,
            suggested_delay_secs: 0,
            exhausted: false,
        });
    }

    // 5b. Even without approval, if checks are failing, flag as RedCI.
    if pr_has_failing_checks(pr) {
        return Some(WatchAction {
            kind: WatchActionKind::RedCI,
            pr_number: pr.number,
            pr_title: pr.title.clone(),
            snapshot: None,
            first_unknown_at: None,
            deadline_exceeded: false,
            retries_left: 0,
            suggested_delay_secs: 0,
            exhausted: false,
        });
    }

    // 5c. Mergeable state "dirty" → conflict.
    if pr.mergeable_state == "dirty" {
        return Some(WatchAction {
            kind: WatchActionKind::Conflict,
            pr_number: pr.number,
            pr_title: pr.title.clone(),
            snapshot: None,
            first_unknown_at: None,
            deadline_exceeded: false,
            retries_left: 0,
            suggested_delay_secs: 0,
            exhausted: false,
        });
    }

    // 6. No changes since last check and we have retries remaining.
    if let Some(p) = prior {
        if has_no_changes(pr, p) {
            if let Some(b) = budget {
                if b.remaining() > 0 {
                    return Some(WatchAction {
                        kind: WatchActionKind::RetryCheck,
                        pr_number: pr.number,
                        pr_title: pr.title.clone(),
                        snapshot: None,
                        first_unknown_at: p.first_unknown_at.clone(),
                        deadline_exceeded: false,
                        retries_left: b.remaining() - 1,
                        suggested_delay_secs: 0,
                        exhausted: false,
                    });
                }
            }
            // Exhausted retries — mark as stuck.
            return Some(WatchAction {
                kind: WatchActionKind::Stuck,
                pr_number: pr.number,
                pr_title: pr.title.clone(),
                snapshot: None,
                first_unknown_at: p.first_unknown_at.clone(),
                deadline_exceeded: true,
                retries_left: 0,
                suggested_delay_secs: 0,
                exhausted: true,
            });
        }
    }

    // 7. All good, nothing to do.
    None
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// True when the PR snapshot is materially unchanged since the prior marker.
fn has_no_changes(pr: &PullRequestDetail, prior: &PriorWatchMarker) -> bool {
    pr.head_sha == prior.head_sha && ReviewDecision::from_github_string(&pr.review_decision) == prior.review_decision
}

// ---------------------------------------------------------------------------
// CI check helpers
// ---------------------------------------------------------------------------

/// Check if any CI checks on this PR are in a failing / error / cancelled state.
///
/// Parses the raw `checks` field (`Vec<HashMap<String, Value>>`) for check run
/// conclusions. Returns `true` when at least one check has a failure conclusion.
pub fn pr_has_failing_checks(pr: &PullRequestDetail) -> bool {
    pr.checks.iter().any(|check| {
        check.get("conclusion").and_then(|c| c.as_str()).is_some_and(|c| {
            matches!(c, "failure" | "cancelled" | "timed_out" | "action_required" | "startup_failure" | "stale")
        })
    })
}

/// Check if any CI checks on this PR are still pending/in-progress.
pub fn pr_has_pending_checks(pr: &PullRequestDetail) -> bool {
    pr.checks.iter().any(|check| {
        check.get("status").and_then(|s| s.as_str()).is_some_and(|s| matches!(s, "queued" | "in_progress" | "waiting"))
    })
}

/// Extract the list of failed check names from PR checks.
pub fn pr_failing_check_names(pr: &PullRequestDetail) -> Vec<String> {
    pr.checks
        .iter()
        .filter(|check| {
            check.get("conclusion").and_then(|c| c.as_str()).is_some_and(|c| {
                matches!(c, "failure" | "cancelled" | "timed_out" | "action_required" | "startup_failure" | "stale")
            })
        })
        .filter_map(|check| check.get("name").and_then(|n| n.as_str()).map(|n| n.to_string()))
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_pr() -> PullRequestDetail {
        PullRequestDetail {
            number: 42,
            title: "feat: add widget".into(),
            body: "Closes #1".into(),
            url: "https://github.com/owner/repo/pull/42".into(),
            state: "open".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-02T00:00:00Z".into(),
            closed_at: String::new(),
            is_draft: false,
            review_decision: "APPROVED".into(),
            labels: vec![],
            head_ref_name: "feat/widget".into(),
            base_ref_name: "main".into(),
            head_sha: "abc123".into(),
            base_sha: "def456".into(),
            author: "user".into(),
            author_association: "MEMBER".into(),
            comment_count: 0,
            review_requests: vec![],
            review_request_users: vec![],
            has_conflicts: false,
            comments: vec![],
            issue_comments: vec![],
            reviews: vec![],
            checks: vec![],
            mergeable: Some(true),
            mergeable_state: "clean".into(),
            merged_at: String::new(),
            auto_merge: None,
        }
    }

    #[test]
    fn test_merged_pr() {
        let mut pr = sample_pr();
        pr.state = "closed".into();
        pr.merged_at = "2026-01-03T00:00:00Z".into();
        let action = classify_pr(&pr, None, None);
        assert!(action.is_some());
        assert_eq!(action.unwrap().kind, WatchActionKind::MarkMerged);
    }

    #[test]
    fn test_closed_pr() {
        let mut pr = sample_pr();
        pr.state = "closed".into();
        let action = classify_pr(&pr, None, None);
        assert!(action.is_some());
        assert_eq!(action.unwrap().kind, WatchActionKind::ClosePR);
    }

    #[test]
    fn test_changes_requested() {
        let mut pr = sample_pr();
        pr.review_decision = "CHANGES_REQUESTED".into();
        let action = classify_pr(&pr, None, None);
        assert!(action.is_some());
        assert_eq!(action.unwrap().kind, WatchActionKind::ReengageReview);
    }

    #[test]
    fn test_merge_ready() {
        let pr = sample_pr();
        let action = classify_pr(&pr, None, None);
        assert!(action.is_some());
        assert_eq!(action.unwrap().kind, WatchActionKind::MergeReady);
    }

    #[test]
    fn test_no_changes_stuck() {
        // PR that is NOT merge-ready (review_decision != APPROVED)
        let mut pr = sample_pr();
        pr.review_decision = "REVIEW_REQUIRED".into();
        let prior = PriorWatchMarker {
            pr_number: 42,
            head_sha: "abc123".into(),
            retries: 0,
            first_unknown_at: None,
            next_retry_at: None,
            review_decision: Some(ReviewDecision::ReviewRequired),
        };
        let budget_exhausted = RetryBudget::new(3, 3..3);
        let action = classify_pr(&pr, Some(&prior), Some(&budget_exhausted));
        assert!(action.is_some());
        assert_eq!(action.unwrap().kind, WatchActionKind::Stuck);
    }

    #[test]
    fn test_no_changes_retry() {
        // PR that is NOT merge-ready (review_decision != APPROVED)
        let mut pr = sample_pr();
        pr.review_decision = "REVIEW_REQUIRED".into();
        let prior = PriorWatchMarker {
            pr_number: 42,
            head_sha: "abc123".into(),
            retries: 0,
            first_unknown_at: None,
            next_retry_at: None,
            review_decision: Some(ReviewDecision::ReviewRequired),
        };
        let budget = RetryBudget::new(3, 0..3);
        let action = classify_pr(&pr, Some(&prior), Some(&budget));
        assert!(action.is_some());
        assert_eq!(action.unwrap().kind, WatchActionKind::RetryCheck);
    }

    #[test]
    fn test_review_required_not_yet_approved() {
        let mut pr = sample_pr();
        pr.review_decision = "REVIEW_REQUIRED".into();
        let action = classify_pr(&pr, None, None);
        assert!(action.is_none());
    }
}
