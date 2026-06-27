use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// CheckoutMode
// ---------------------------------------------------------------------------

/// Whether a worktree is checked out on a branch or detached HEAD.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckoutMode {
    Branch,
    Detached,
}

impl fmt::Display for CheckoutMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CheckoutMode::Branch => write!(f, "branch"),
            CheckoutMode::Detached => write!(f, "detached"),
        }
    }
}

// ---------------------------------------------------------------------------
// GatewayOptions
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct GatewayOptions {
    /// Path to the git binary. Defaults to "git".
    pub git_path: String,
    /// Optional storage for DB persistence of worktree records.
    pub repos: Option<Arc<looper_storage::repos::Repositories>>,
    /// Clock function. Defaults to Utc::now.
    pub now: fn() -> chrono::DateTime<chrono::Utc>,
}

impl Default for GatewayOptions {
    fn default() -> Self {
        Self { git_path: "git".to_string(), repos: None, now: chrono::Utc::now }
    }
}

// ---------------------------------------------------------------------------
// Input / Output structs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct CreateWorktreeInput {
    pub project_id: String,
    pub repo_path: String,
    pub worktree_root: String,
    pub branch: String,
    pub base_branch: Option<String>,
    pub start_point: Option<String>,
    pub pr_number: Option<i64>,
    pub checkout_mode: CheckoutMode,
    pub protected_branches: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct CreateWorktreeResult {
    pub record: looper_storage::WorktreeRecord,
    pub recovered: bool,
}

#[derive(Debug, Clone)]
pub struct RestoreWorktreeInput {
    pub project_id: String,
    pub repo_path: String,
    pub worktree_root: String,
    pub branch: String,
    pub base_branch: Option<String>,
    pub checkout_mode: CheckoutMode,
}

#[derive(Debug, Clone)]
pub struct CleanupWorktreeInput {
    pub repo_path: String,
    pub worktree_path: String,
    pub branch: String,
    pub protected_branches: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PrepareWorktreeInput {
    pub worktree_path: String,
    pub remote: Option<String>,
    pub target_spec: String,
    pub reset_ref: String,
}

#[derive(Debug, Clone)]
pub struct PrepareWorktreeResult {
    pub head_sha: String,
    pub was_dirty: bool,
}

#[derive(Debug, Clone)]
pub struct InspectHeadInput {
    pub worktree_path: String,
    pub base_ref: Option<String>,
}

#[derive(Debug, Clone)]
pub struct InspectHeadResult {
    pub head_sha: String,
    pub new_commits: Vec<String>,
    pub changed_files: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct CommitInput {
    pub worktree_path: String,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct CommitResult {
    pub head_sha: String,
}

#[derive(Debug, Clone)]
pub struct PushInput {
    pub worktree_path: String,
    pub remote: String,
    pub branch: String,
    pub expected_head_sha: Option<String>,
    pub protected_branches: Vec<String>,
    pub set_upstream: bool,
}

// ---------------------------------------------------------------------------
// WorktreeListEntry (from `git worktree list --porcelain`)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct WorktreeListEntry {
    pub path: String,
    /// Branch name without "refs/heads/" prefix, if not detached.
    pub branch: Option<String>,
    pub head_sha: Option<String>,
    pub bare: bool,
}

// ---------------------------------------------------------------------------
// Branch naming helpers (data-only, no external deps)
// ---------------------------------------------------------------------------

/// Sanitize a branch name for use as a directory name.
/// Keeps `[a-zA-Z0-9._-]`, replaces everything else with `-`.
pub fn sanitize_branch_name(name: &str) -> String {
    name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' { c } else { '-' }).collect()
}

/// Build the worktree directory name from input.
pub fn build_worktree_directory_name(input: &CreateWorktreeInput) -> String {
    let sanitized = sanitize_branch_name(&input.project_id);
    if let Some(pr) = input.pr_number {
        if input.checkout_mode == CheckoutMode::Detached {
            format!("looper-fix-{}-pr-{}-detached", sanitized, pr)
        } else {
            format!("looper-fix-{}-pr-{}", sanitized, pr)
        }
    } else {
        sanitize_branch_name(&input.branch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_input(project_id: &str, branch: &str, pr: Option<i64>, mode: CheckoutMode) -> CreateWorktreeInput {
        CreateWorktreeInput {
            project_id: project_id.to_string(),
            repo_path: "/repo".to_string(),
            worktree_root: "/wt".to_string(),
            branch: branch.to_string(),
            base_branch: None,
            start_point: None,
            pr_number: pr,
            checkout_mode: mode,
            protected_branches: vec![],
        }
    }

    #[test]
    fn test_sanitize_branch_name() {
        assert_eq!(sanitize_branch_name("feature/my-thing"), "feature-my-thing");
        assert_eq!(sanitize_branch_name("fix/JIRA-123_bug"), "fix-JIRA-123_bug");
        assert_eq!(sanitize_branch_name("simple-name"), "simple-name");
        assert_eq!(sanitize_branch_name("weird:name@here"), "weird-name-here");
        assert_eq!(sanitize_branch_name(""), "");
    }

    #[test]
    fn test_directory_name_for_pr_detached() {
        let input = make_input("my-proj", "feature/foo", Some(42), CheckoutMode::Detached);
        assert_eq!(build_worktree_directory_name(&input), "looper-fix-my-proj-pr-42-detached");
    }

    #[test]
    fn test_directory_name_for_pr_branch_mode() {
        let input = make_input("my-proj", "feature/foo", Some(42), CheckoutMode::Branch);
        assert_eq!(build_worktree_directory_name(&input), "looper-fix-my-proj-pr-42");
    }

    #[test]
    fn test_directory_name_for_branch() {
        let input = make_input("my-proj", "feature/my-thing", None, CheckoutMode::Branch);
        assert_eq!(build_worktree_directory_name(&input), "feature-my-thing");
    }
}
