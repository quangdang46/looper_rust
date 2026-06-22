use std::collections::HashSet;

use crate::types::Lane;

/// Determine which lanes a webhook event should be dispatched to.
///
/// Routing rules:
/// - pull_request: review_requested → [reviewer]
/// - pull_request: labeled/unlabeled → [fixer]
/// - pull_request: opened/reopened/ready_for_review/synchronize → [reviewer, fixer]
/// - pull_request_review / pull_request_review_comment → [fixer]
/// - check_run (completed, failing conclusion) → [fixer]
/// - push (non-delete, branch) → [fixer as base_branch]
/// - issue_comment → ignore
/// - other → ignore
pub fn route_event(event_type: &str, action: Option<&str>) -> RoutingDecision {
    match event_type {
        "pull_request" => match action {
            Some("review_requested") => lanes([Lane::Reviewer]),
            Some("labeled" | "unlabeled") => lanes([Lane::Fixer]),
            Some("opened" | "reopened" | "ready_for_review" | "synchronize") => {
                lanes([Lane::Reviewer, Lane::Fixer])
            }
            Some(_) => ignore(),
            None => ignore(),
        },
        "pull_request_review" | "pull_request_review_comment" => lanes([Lane::Fixer]),
        "check_run" => match action {
            Some("completed") => RoutingDecision::CheckRun,
            _ => ignore(),
        },
        "push" => match action {
            Some("branch") | None => RoutingDecision::Push,
            _ => ignore(),
        },
        "issue_comment" => ignore(),
        _ => ignore(),
    }
}

/// Decisions the router can return.
#[derive(Debug, Clone)]
pub enum RoutingDecision {
    /// Ignore this event entirely.
    Ignore,
    /// Route to specific lanes for a pull-request-type event.
    PullRequest(HashSet<Lane>),
    /// Route to fixer lane for a branch push event.
    Push,
    /// Route to fixer lane for a check_run completed event.
    CheckRun,
}

impl RoutingDecision {
    pub fn is_ignored(&self) -> bool {
        matches!(self, RoutingDecision::Ignore)
    }

    pub fn lanes(&self) -> HashSet<Lane> {
        match self {
            RoutingDecision::PullRequest(ls) => ls.clone(),
            RoutingDecision::Push | RoutingDecision::CheckRun => {
                let mut s = HashSet::new();
                s.insert(Lane::Fixer);
                s
            }
            RoutingDecision::Ignore => HashSet::new(),
        }
    }
}

fn lanes(arr: impl IntoIterator<Item = Lane>) -> RoutingDecision {
    RoutingDecision::PullRequest(arr.into_iter().collect())
}

fn ignore() -> RoutingDecision {
    RoutingDecision::Ignore
}

/// Check if a check_run conclusion indicates a failure.
pub fn is_failing_conclusion(conclusion: &str) -> bool {
    matches!(
        conclusion.to_uppercase().as_str(),
        "FAILURE" | "FAILED" | "ERROR" | "TIMED_OUT" | "ACTION_REQUIRED"
    )
}

/// Check if an error is likely transient (for retry logic).
pub fn is_transient_error(err: &crate::error::WebhookError) -> bool {
    err.is_transient()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pull_request_review_requested() {
        let d = route_event("pull_request", Some("review_requested"));
        assert!(!d.is_ignored());
        let lanes = d.lanes();
        assert!(lanes.contains(&Lane::Reviewer));
        assert!(!lanes.contains(&Lane::Fixer));
    }

    #[test]
    fn test_pull_request_labeled() {
        let d = route_event("pull_request", Some("labeled"));
        let lanes = d.lanes();
        assert!(lanes.contains(&Lane::Fixer));
        assert!(!lanes.contains(&Lane::Reviewer));
    }

    #[test]
    fn test_pull_request_opened() {
        let d = route_event("pull_request", Some("opened"));
        let lanes = d.lanes();
        assert!(lanes.contains(&Lane::Reviewer));
        assert!(lanes.contains(&Lane::Fixer));
    }

    #[test]
    fn test_pull_request_unknown_action() {
        let d = route_event("pull_request", Some("unknown_action"));
        assert!(d.is_ignored());
    }

    #[test]
    fn test_pull_request_no_action() {
        let d = route_event("pull_request", None);
        assert!(d.is_ignored());
    }

    #[test]
    fn test_pull_request_review() {
        let d = route_event("pull_request_review", None);
        let lanes = d.lanes();
        assert!(lanes.contains(&Lane::Fixer));
    }

    #[test]
    fn test_check_run_completed() {
        let d = route_event("check_run", Some("completed"));
        assert!(matches!(d, RoutingDecision::CheckRun));
    }

    #[test]
    fn test_check_run_not_completed() {
        let d = route_event("check_run", Some("created"));
        assert!(d.is_ignored());
    }

    #[test]
    fn test_push() {
        let d = route_event("push", None);
        assert!(matches!(d, RoutingDecision::Push));
    }

    #[test]
    fn test_issue_comment() {
        let d = route_event("issue_comment", Some("created"));
        assert!(d.is_ignored());
    }

    #[test]
    fn test_unknown_event() {
        let d = route_event("unknown_event", None);
        assert!(d.is_ignored());
    }

    #[test]
    fn test_failing_conclusions() {
        assert!(is_failing_conclusion("failure"));
        assert!(is_failing_conclusion("FAILURE"));
        assert!(is_failing_conclusion("Failed"));
        assert!(is_failing_conclusion("error"));
        assert!(is_failing_conclusion("timed_out"));
        assert!(is_failing_conclusion("action_required"));
        assert!(!is_failing_conclusion("success"));
        assert!(!is_failing_conclusion("neutral"));
        assert!(!is_failing_conclusion("skipped"));
    }
}
