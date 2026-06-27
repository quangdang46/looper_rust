use crate::types::{
    has_exact_target, parse_target_label, ClaimDecision, GitHubIdentity, MatchMode, NetworkMode, ProjectPolicy,
};

/// Evaluate whether a node can claim a worker role on a PR.
///
/// # Worker conditions
/// 1. If mode is not `Routed` → allowed (local mode)
/// 2. Must have label `looper:worker-ready`
/// 3. Must have exactly one `looper:target:<node_name>` label matching the local node
/// 4. Local GitHub identity must be in the PR assignees list
pub fn evaluate_worker(policy: &ProjectPolicy, labels: &[String], assignees: &[GitHubIdentity]) -> ClaimDecision {
    if !matches!(policy.mode, NetworkMode::Routed) {
        return ClaimDecision {
            allowed: true,
            reason: String::new(),
            match_mode: MatchMode::None,
            target_label: String::new(),
        };
    }

    // Must have worker-ready label
    if !labels.iter().any(|l| l == "looper:worker-ready") {
        return ClaimDecision {
            allowed: false,
            reason: "PR is not worker-ready: missing 'looper:worker-ready' label".to_string(),
            match_mode: MatchMode::None,
            target_label: String::new(),
        };
    }

    // Must have exactly one target label matching our node
    let targets: Vec<&str> = labels.iter().filter_map(|l| parse_target_label(l)).collect();
    if targets.len() != 1 {
        return ClaimDecision {
            allowed: false,
            reason: format!("expected exactly 1 target label, found {}", targets.len()),
            match_mode: MatchMode::None,
            target_label: String::new(),
        };
    }
    if targets[0] != policy.node_name {
        return ClaimDecision {
            allowed: false,
            reason: format!("target label '{}' does not match local node '{}'", targets[0], policy.node_name),
            match_mode: MatchMode::None,
            target_label: targets[0].to_string(),
        };
    }

    // Check assignee match
    match match_local_identity(policy, assignees) {
        Some(mode) => ClaimDecision {
            allowed: true,
            reason: String::new(),
            match_mode: mode,
            target_label: format!("looper:target:{}", policy.node_name),
        },
        None => ClaimDecision {
            allowed: false,
            reason: "local GitHub identity not found in PR assignees".to_string(),
            match_mode: MatchMode::None,
            target_label: format!("looper:target:{}", policy.node_name),
        },
    }
}

/// Evaluate whether a node can claim a reviewer role on a PR.
///
/// # Reviewer conditions
/// 1. If mode is not `Routed` → allowed (local mode)
/// 2. Must have exactly one `looper:target:<node_name>` label matching local node
/// 3. Local GitHub identity must be in the review request list
pub fn evaluate_reviewer(
    policy: &ProjectPolicy,
    labels: &[String],
    review_requests: &[GitHubIdentity],
) -> ClaimDecision {
    if !matches!(policy.mode, NetworkMode::Routed) {
        return ClaimDecision {
            allowed: true,
            reason: String::new(),
            match_mode: MatchMode::None,
            target_label: String::new(),
        };
    }

    // Must have exactly one target label matching our node
    let targets: Vec<&str> = labels.iter().filter_map(|l| parse_target_label(l)).collect();
    if targets.len() != 1 {
        return ClaimDecision {
            allowed: false,
            reason: format!("expected exactly 1 target label, found {}", targets.len()),
            match_mode: MatchMode::None,
            target_label: String::new(),
        };
    }
    if targets[0] != policy.node_name {
        return ClaimDecision {
            allowed: false,
            reason: format!("target label '{}' does not match local node '{}'", targets[0], policy.node_name),
            match_mode: MatchMode::None,
            target_label: targets[0].to_string(),
        };
    }

    // Check review request match
    match match_local_identity(policy, review_requests) {
        Some(mode) => ClaimDecision {
            allowed: true,
            reason: String::new(),
            match_mode: mode,
            target_label: format!("looper:target:{}", policy.node_name),
        },
        None => ClaimDecision {
            allowed: false,
            reason: "local GitHub identity not found in review requests".to_string(),
            match_mode: MatchMode::None,
            target_label: format!("looper:target:{}", policy.node_name),
        },
    }
}

/// Match the local node's GitHub identity against a list of users.
///
/// Priority:
/// 1. Numeric match (both IDs > 0 and equal)
/// 2. Login fallback (case-insensitive login comparison)
fn match_local_identity(policy: &ProjectPolicy, users: &[GitHubIdentity]) -> Option<MatchMode> {
    for user in users {
        if policy.github_user_id > 0 && user.numeric_id > 0 && policy.github_user_id == user.numeric_id {
            return Some(MatchMode::Numeric);
        }
    }
    for user in users {
        if policy.github_login.to_lowercase() == user.login.to_lowercase() {
            return Some(MatchMode::LoginFallback);
        }
    }
    None
}

/// Compute whether a set of labels needs to be adjusted before dispatching work to this node.
pub fn labels_need_target_adjustment(labels: &[String], node_name: &str) -> bool {
    let expected = format!("looper:target:{}", node_name);
    !has_exact_target(labels, node_name) || labels.iter().any(|l| l == &expected)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_policy() -> ProjectPolicy {
        ProjectPolicy {
            mode: NetworkMode::Routed,
            node_name: "my-node".to_string(),
            github_login: "testuser".to_string(),
            github_user_id: 12345,
        }
    }

    fn make_identity(id: i64, login: &str) -> GitHubIdentity {
        GitHubIdentity { numeric_id: id, login: login.to_string() }
    }

    #[test]
    fn test_local_mode_always_allowed() {
        let policy = ProjectPolicy {
            mode: NetworkMode::Off,
            node_name: "my-node".to_string(),
            github_login: "testuser".to_string(),
            github_user_id: 12345,
        };
        let decision = evaluate_worker(&policy, &[], &[]);
        assert!(decision.allowed);

        let decision = evaluate_reviewer(&policy, &[], &[]);
        assert!(decision.allowed);
    }

    #[test]
    fn test_worker_no_worker_ready_label() {
        let policy = make_policy();
        let decision =
            evaluate_worker(&policy, &["looper:target:my-node".to_string()], &[make_identity(12345, "testuser")]);
        assert!(!decision.allowed);
        assert!(decision.reason.contains("worker-ready"));
    }

    #[test]
    fn test_worker_no_target_label() {
        let policy = make_policy();
        let decision =
            evaluate_worker(&policy, &["looper:worker-ready".to_string()], &[make_identity(12345, "testuser")]);
        assert!(!decision.allowed);
    }

    #[test]
    fn test_worker_wrong_target_label() {
        let policy = make_policy();
        let decision = evaluate_worker(
            &policy,
            &["looper:worker-ready".to_string(), "looper:target:other-node".to_string()],
            &[make_identity(12345, "testuser")],
        );
        assert!(!decision.allowed);
        assert!(decision.reason.contains("other-node"));
    }

    #[test]
    fn test_worker_not_assignee() {
        let policy = make_policy();
        let decision = evaluate_worker(
            &policy,
            &["looper:worker-ready".to_string(), "looper:target:my-node".to_string()],
            &[make_identity(99999, "other")],
        );
        assert!(!decision.allowed);
        assert!(decision.reason.contains("assignees"));
    }

    #[test]
    fn test_worker_numeric_match() {
        let policy = make_policy();
        let decision = evaluate_worker(
            &policy,
            &["looper:worker-ready".to_string(), "looper:target:my-node".to_string()],
            &[make_identity(12345, "testuser")],
        );
        assert!(decision.allowed);
        assert_eq!(decision.match_mode, MatchMode::Numeric);
    }

    #[test]
    fn test_worker_login_fallback() {
        let policy = make_policy();
        let decision = evaluate_worker(
            &policy,
            &["looper:worker-ready".to_string(), "looper:target:my-node".to_string()],
            &[make_identity(0, "TestUser")],
        );
        assert!(decision.allowed);
        assert_eq!(decision.match_mode, MatchMode::LoginFallback);
    }

    #[test]
    fn test_reviewer_match() {
        let policy = make_policy();
        let decision =
            evaluate_reviewer(&policy, &["looper:target:my-node".to_string()], &[make_identity(12345, "testuser")]);
        assert!(decision.allowed);
    }

    #[test]
    fn test_reviewer_no_match() {
        let policy = make_policy();
        let decision =
            evaluate_reviewer(&policy, &["looper:target:my-node".to_string()], &[make_identity(99999, "other")]);
        assert!(!decision.allowed);
    }
}
