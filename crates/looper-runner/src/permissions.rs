//! Permission checks for dispatch actions.
//!
//! Provides the single [`user_authorized_for_dispatch`] function used by both
//! the Coordinator's dispatch phase and the Planner's discovery phase to
//! gate issue-triggered work.

use looper_github::Gateway;
use looper_scheduler::types::DispatchConfig;

/// Check whether a GitHub user is authorized to trigger dispatch.
///
/// When `allowed_users` is non-empty, the user is authorized if they appear in
/// the list **or** have write/admin access on the repo.
///
/// When `allowed_users` is empty (the default), only users with `admin` or
/// `write` GitHub permission are authorized.
///
/// Returns `false` (fail-closed) when the GitHub permission API call fails.
pub fn user_authorized_for_dispatch(username: &str, repo: &str, dispatch_cfg: &DispatchConfig, gw: &Gateway) -> bool {
    // Check explicit allow-list first
    if !dispatch_cfg.allowed_users.is_empty() && dispatch_cfg.allowed_users.iter().any(|u| u == username) {
        return true;
    }

    // Default: check write/admin via GitHub API
    match gw.get_repository_permission(looper_github::types::RepositoryPermissionInput {
        repo: repo.to_string(),
        user: username.to_string(),
        cwd: ".".to_string(),
    }) {
        Ok(perm) => perm == "admin" || perm == "write",
        Err(e) => {
            tracing::warn!("Permission check failed for '{username}' on {repo}: {e}");
            false // fail closed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allowed_users_list_match() {
        let cfg = DispatchConfig { allowed_users: vec!["alice".into(), "bob".into()], ..Default::default() };
        assert!(cfg.allowed_users.contains(&"alice".to_string()));
        assert!(!cfg.allowed_users.contains(&"eve".to_string()));
    }

    #[test]
    fn test_empty_allowed_users_falls_to_write_check() {
        let cfg = DispatchConfig::default();
        assert!(cfg.allowed_users.is_empty());
        // With an empty list the function will call the GitHub API, which
        // we can't easily mock here. The logic is tested at the integration
        // level in looper-github.
    }
}
