use crate::error::{GitError, ProtectedBranchError, Result};
use std::path::Path;

/// Maximum symlink resolution depth to prevent infinite loops.
const MAX_SYMLINK_DEPTH: u32 = 255;

/// Check that `branch` is not in the protected list.
/// The protected list typically includes base branches, main/master, etc.
pub fn assert_writable_branch(branch: &str, protected_branches: &[String]) -> Result<()> {
    if protected_branches.iter().any(|b| b == branch) {
        return Err(GitError::ProtectedBranch(ProtectedBranchError {
            branch: branch.to_string(),
        }));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Worktree path safety validation
// ---------------------------------------------------------------------------

/// Input for worktree path safety checks.
#[derive(Debug, Clone)]
pub struct SafetyCheckInput {
    /// The worktree path to validate.
    pub path: String,
    /// The repository path (worktree must NOT equal this).
    pub repo_path: Option<String>,
    /// The worktree root path (worktree must be under this).
    pub worktree_root: Option<String>,
}

/// Validate that a worktree path is safe to operate on.
///
/// Checks:
/// 1. Path must not be empty.
/// 2. Path must not equal repo path (if provided).
/// 3. If worktree root is set: path must not equal root, path must be under root.
/// 4. Symlink-aware path normalization (recursive, depth-limited).
/// 5. Resolves relative paths to absolute.
/// 6. Resolves all symlinks in the path.
pub fn validate_worktree_path(input: &SafetyCheckInput) -> Result<()> {
    // 1. Path must not be empty
    if input.path.is_empty() {
        return Err(GitError::SafetyCheckFailed {
            detail: "worktree path is empty".to_string(),
        });
    }

    // 2. Path must not equal repo path
    if let Some(repo_path) = &input.repo_path {
        let repo_canonical = canonicalize_safe(repo_path)?;
        let wt_canonical = canonicalize_safe(&input.path)?;
        if wt_canonical == repo_canonical {
            return Err(GitError::SafetyCheckFailed {
                detail: format!(
                    "worktree path '{}' is the same as repo path '{}'",
                    input.path, repo_path
                ),
            });
        }
    }

    // 3. Worktree root checks
    if let Some(root) = &input.worktree_root {
        if root.is_empty() {
            return Err(GitError::SafetyCheckFailed {
                detail: "worktree root is empty".to_string(),
            });
        }
        let root_canonical = canonicalize_safe(root)?;
        let wt_canonical = canonicalize_safe(&input.path)?;

        if wt_canonical == root_canonical {
            return Err(GitError::SafetyCheckFailed {
                detail: format!(
                    "worktree path '{}' is the same as worktree root '{}'",
                    input.path, root
                ),
            });
        }

        if !wt_canonical.starts_with(&root_canonical) {
            return Err(GitError::SafetyCheckFailed {
                detail: format!(
                    "worktree path '{}' is not under worktree root '{}'",
                    input.path, root
                ),
            });
        }
    }

    Ok(())
}

/// Canonicalize a path safely: resolve to absolute, follow symlinks with depth limit.
fn canonicalize_safe(path: &str) -> Result<String> {
    let p = Path::new(path);

    // Resolve to absolute first
    let absolute = if p.is_relative() {
        let cwd = std::env::current_dir().map_err(GitError::Io)?;
        cwd.join(p)
    } else {
        p.to_path_buf()
    };

    // Resolve symlinks recursively with depth limit
    let resolved = resolve_symlinks(&absolute, 0)?;

    Ok(resolved.to_string_lossy().to_string())
}

/// Recursively resolve symlinks in a path.
fn resolve_symlinks(path: &Path, depth: u32) -> Result<std::path::PathBuf> {
    if depth > MAX_SYMLINK_DEPTH {
        return Err(GitError::SafetyCheckFailed {
            detail: format!(
                "symlink resolution exceeded max depth ({}) for '{}'",
                MAX_SYMLINK_DEPTH,
                path.display()
            ),
        });
    }

    // Read the symlink target if it's a symlink
    if path.is_symlink() {
        let target = std::fs::read_link(path).map_err(GitError::Io)?;
        let resolved = if target.is_absolute() {
            target
        } else {
            // Relative symlink — resolve against parent directory
            let parent = path.parent().unwrap_or(Path::new("."));
            parent.join(target)
        };
        return resolve_symlinks(&resolved, depth + 1);
    }

    // For components, resolve each one
    let mut result = std::path::PathBuf::new();
    for component in path.components() {
        let candidate = result.join(component);
        if candidate.is_symlink() {
            let target = std::fs::read_link(&candidate).map_err(GitError::Io)?;
            let resolved = if target.is_absolute() {
                target
            } else {
                // Relative — resolve against current result
                result.join(target)
            };
            result = resolve_symlinks(&resolved, depth + 1)?;
        } else {
            result = candidate;
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_assert_writable_branch_allows_normal() {
        assert!(assert_writable_branch("feature/foo", &["main".to_string(), "master".to_string()]).is_ok());
    }

    #[test]
    fn test_assert_writable_branch_blocks_protected() {
        let err = assert_writable_branch("main", &["main".to_string()]).unwrap_err();
        assert!(err.to_string().contains("protected"));
    }

    #[test]
    fn test_assert_writable_branch_blocks_any_protected() {
        let err = assert_writable_branch("master", &["main".to_string(), "master".to_string()]).unwrap_err();
        assert!(err.to_string().contains("protected"));
    }

    #[test]
    fn test_validate_empty_path() {
        let input = SafetyCheckInput {
            path: "".to_string(),
            repo_path: None,
            worktree_root: None,
        };
        assert!(validate_worktree_path(&input).is_err());
    }
}
