use std::fmt;
use thiserror::Error;

/// Branch is protected — write operations not permitted.
#[derive(Debug, Clone)]
pub struct ProtectedBranchError {
    pub branch: String,
}

impl fmt::Display for ProtectedBranchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "branch '{}' is protected and cannot be modified", self.branch)
    }
}

impl std::error::Error for ProtectedBranchError {}

/// Remote head changed unexpectedly during push.
#[derive(Debug, Clone)]
pub struct RemoteHeadChangedError {
    pub branch: String,
    pub expected_head_sha: String,
    pub actual_head_sha: String,
}

impl fmt::Display for RemoteHeadChangedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "remote head changed for '{}': expected {} but got {}",
            self.branch, self.expected_head_sha, self.actual_head_sha,
        )
    }
}

impl std::error::Error for RemoteHeadChangedError {}

/// Error executing a git command.
#[derive(Debug, Error)]
pub enum GitError {
    #[error("git command exited with code {exit_code}: {stderr}")]
    CommandError {
        exit_code: i32,
        stderr: String,
    },

    #[error("git command timed out: {command}")]
    Timeout {
        command: String,
    },

    #[error("worktree path is invalid: {detail}")]
    InvalidWorktreePath {
        detail: String,
    },

    #[error("worktree not found: {path}")]
    WorktreeNotFound {
        path: String,
    },

    #[error("worktree is dirty: {path}")]
    DirtyWorktree {
        path: String,
    },

    #[error("protected branch: {0}")]
    ProtectedBranch(#[from] ProtectedBranchError),

    #[error("remote head changed: {0}")]
    RemoteHeadChanged(#[from] RemoteHeadChangedError),

    #[error("branch not found: {branch}")]
    BranchNotFound {
        branch: String,
    },

    #[error("remote not found: {remote}")]
    RemoteNotFound {
        remote: String,
    },

    #[error("worktree safety check failed: {detail}")]
    SafetyCheckFailed {
        detail: String,
    },

    #[error("storage error: {0}")]
    Storage(#[from] looper_storage::StorageError),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(String),
}

impl From<String> for GitError {
    fn from(s: String) -> Self {
        GitError::Other(s)
    }
}

impl From<&str> for GitError {
    fn from(s: &str) -> Self {
        GitError::Other(s.to_string())
    }
}

/// Result alias for looper-git operations.
pub type Result<T> = std::result::Result<T, GitError>;

// ---------------------------------------------------------------------------
// Error pattern matching — mirrors the Go regex patterns
// ---------------------------------------------------------------------------

/// Check if a git error message indicates a missing/nonexistent worktree.
pub fn is_missing_worktree_error(stderr: &str) -> bool {
    let lowered = stderr.to_lowercase();
    lowered.contains("is not a working tree")
        || lowered.contains("does not exist")
        || lowered.contains("not found")
        || lowered.contains("no such file")
}

/// Check if a git push error indicates a remote conflict.
pub fn is_push_conflict_error(stderr: &str) -> bool {
    let lowered = stderr.to_lowercase();
    lowered.contains("stale info")
        || lowered.contains("non-fast-forward")
        || lowered.contains("failed to push")
        || lowered.contains("rejected")
}

/// Check if a git fetch error is a lock race that should be retried.
pub fn is_fetch_lock_race(stderr: &str) -> bool {
    let lowered = stderr.to_lowercase();
    lowered.contains("cannot lock ref") && lowered.contains("but expected")
}

/// Check exit code semantics — a non-zero exit may still be "expected".
pub fn is_expected_exit(command: &str, exit_code: i32) -> bool {
    match command {
        // `git show-ref --quiet` exits 1 when ref doesn't exist
        // `git config --get` exits 1 when key not found
        // `git merge-base --is-ancestor` exits 1 when not ancestor
        cmd if cmd.contains("show-ref --quiet")
            || cmd.contains("config --get")
            || cmd.contains("merge-base --is-ancestor") =>
        {
            exit_code == 1
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_missing_worktree_patterns() {
        assert!(is_missing_worktree_error("fatal: '<path>' is not a working tree"));
        assert!(is_missing_worktree_error("fatal: '<path>' does not exist"));
        assert!(is_missing_worktree_error("path not found"));
        assert!(is_missing_worktree_error("no such file or directory"));
        assert!(!is_missing_worktree_error("everything up-to-date"));
    }

    #[test]
    fn test_push_conflict_patterns() {
        assert!(is_push_conflict_error(
            "! [rejected] branch -> branch (non-fast-forward)"
        ));
        assert!(is_push_conflict_error("failed to push some refs"));
        assert!(is_push_conflict_error("stale info"));
        assert!(!is_push_conflict_error("everything up-to-date"));
    }

    #[test]
    fn test_fetch_lock_race() {
        assert!(is_fetch_lock_race(
            "cannot lock ref 'refs/heads/main': 'refs/heads/main' but expected 'abc123'"
        ));
        assert!(!is_fetch_lock_race("cannot lock ref 'refs/heads/main'"));
    }

    #[test]
    fn test_expected_exit_codes() {
        assert!(is_expected_exit("git show-ref --quiet refs/heads/main", 1));
        assert!(is_expected_exit("git config --get remote.origin.url", 1));
        assert!(is_expected_exit("git merge-base --is-ancestor a b", 1));
        assert!(!is_expected_exit("git fetch origin main", 1));
    }
}
