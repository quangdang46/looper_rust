use crate::error::{GitError, Result};

/// Default remote name.
pub const DEFAULT_REMOTE: &str = "origin";

// ---------------------------------------------------------------------------
// Branch existence resolution
// ---------------------------------------------------------------------------

/// Resolve the best start-point ref for a detached worktree.
/// Tries remote branch first, then local branch.
pub async fn resolve_detached_start_point_ref(
    repo_path: &str,
    branch: &str,
) -> Result<Option<String>> {
    has_remote(repo_path, DEFAULT_REMOTE).await?;

    // Try to fetch the remote branch first (best effort)
    let _ = fetch_ref(repo_path, DEFAULT_REMOTE, branch).await;

    // Check remote branch
    if remote_branch_exists(repo_path, DEFAULT_REMOTE, branch).await? {
        return Ok(Some(format!("{}/{}", DEFAULT_REMOTE, branch)));
    }

    // Check local branch
    if local_branch_exists(repo_path, branch).await? {
        return Ok(Some(branch.to_string()));
    }

    Ok(None)
}

/// Resolve the best start-point ref for an attached worktree.
/// Tries remote branch first, then falls back to base branch.
pub async fn resolve_attached_start_point(
    repo_path: &str,
    branch: &str,
    base_branch: Option<&str>,
) -> Result<String> {
    // Try remote branch first
    if remote_branch_exists(repo_path, DEFAULT_REMOTE, branch).await? {
        return Ok(format!("{}/{}", DEFAULT_REMOTE, branch));
    }

    // Fall back to base branch
    if let Some(base) = base_branch {
        if local_branch_exists(repo_path, base).await? || remote_branch_exists(repo_path, DEFAULT_REMOTE, base).await? {
            return Ok(base.to_string());
        }
    }

    Err(GitError::BranchNotFound {
        branch: branch.to_string(),
    })
}

// ---------------------------------------------------------------------------
// Git existence checks
// ---------------------------------------------------------------------------

/// Check if a local branch exists.
pub async fn local_branch_exists(repo_path: &str, branch: &str) -> Result<bool> {
    let output = tokio::process::Command::new("git")
        .args([
            "show-ref",
            "--quiet",
            "--verify",
            &format!("refs/heads/{}", branch),
        ])
        .current_dir(repo_path)
        .output()
        .await?;

    Ok(output.status.success())
}

/// Check if a remote branch exists.
pub async fn remote_branch_exists(repo_path: &str, remote: &str, branch: &str) -> Result<bool> {
    let output = tokio::process::Command::new("git")
        .args([
            "show-ref",
            "--quiet",
            "--verify",
            &format!("refs/remotes/{}/{}", remote, branch),
        ])
        .current_dir(repo_path)
        .output()
        .await?;

    Ok(output.status.success())
}

/// Check if the repository has a remote.
pub async fn has_remote(repo_path: &str, remote: &str) -> Result<bool> {
    let output = tokio::process::Command::new("git")
        .args(["config", "--get", &format!("remote.{}.url", remote)])
        .current_dir(repo_path)
        .output()
        .await?;

    Ok(output.status.success())
}

/// Check if a worktree path is detached (HEAD points to a commit, not a branch).
pub async fn is_detached(worktree_path: &str) -> Result<bool> {
    let output = run_git_cmd(worktree_path, ["rev-parse", "--abbrev-ref", "HEAD"]).await?;
    Ok(output.trim() == "HEAD")
}

/// Fetch a remote ref (used for detached start point resolution).
pub async fn fetch_ref(repo_path: &str, remote: &str, branch: &str) -> Result<()> {
    let spec = format!("+refs/heads/{}:refs/remotes/{}/{}", branch, remote, branch);
    run_git_cmd(repo_path, ["fetch", remote, &spec]).await?;
    Ok(())
}

/// Get the current HEAD SHA for a worktree.
pub async fn get_head_sha(repo_path: &str) -> Result<String> {
    let output = run_git_cmd(repo_path, ["rev-parse", "HEAD"]).await?;
    Ok(output.trim().to_string())
}

// ---------------------------------------------------------------------------
// Core git command runner
// ---------------------------------------------------------------------------

/// Run a git command and return stdout, or an error with details.
pub async fn run_git_cmd<I, S>(cwd: &str, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let output = tokio::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .await?;

    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();

    if !output.status.success() {
        return Err(GitError::CommandError {
            exit_code: output.status.code().unwrap_or(-1),
            stderr: stderr.trim().to_string(),
        });
    }

    Ok(stdout)
}

/// Run a git command with retry for fetch lock races.
pub async fn run_git_with_retry<I, S>(cwd: &str, args: I) -> Result<String>
where
    I: IntoIterator<Item = S> + Clone,
    S: AsRef<std::ffi::OsStr>,
{
    let max_attempts = 3;
    let delays = [50u64, 100u64];

    for attempt in 0..max_attempts {
        let output = tokio::process::Command::new("git")
            .args(args.clone())
            .current_dir(cwd)
            .output()
            .await?;

        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();

        // Check if this is a retryable fetch lock race
        if !output.status.success()
            && attempt < max_attempts - 1
            && crate::error::is_fetch_lock_race(&stderr)
        {
            let delay_ms = delays.get(attempt).copied().unwrap_or(100);
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            continue;
        }

        if !output.status.success() {
            return Err(GitError::CommandError {
                exit_code: output.status.code().unwrap_or(-1),
                stderr: stderr.trim().to_string(),
            });
        }

        return Ok(stdout);
    }

    Err(GitError::Other(
        "max retry attempts exceeded for fetch command".to_string(),
    ))
}

// ---------------------------------------------------------------------------
// Porcelain parser
// ---------------------------------------------------------------------------

/// Parse `git worktree list --porcelain` output into entries.
pub fn parse_worktree_list(output: &str) -> Vec<crate::types::WorktreeListEntry> {
    let mut entries = Vec::new();
    let mut current = crate::types::WorktreeListEntry {
        path: String::new(),
        branch: None,
        head_sha: None,
        bare: false,
    };
    let mut in_entry = false;

    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            if in_entry {
                entries.push(std::mem::take(&mut current));
                current = crate::types::WorktreeListEntry {
                    path: String::new(),
                    branch: None,
                    head_sha: None,
                    bare: false,
                };
                in_entry = false;
            }
            continue;
        }

        in_entry = true;

        if let Some(path) = line.strip_prefix("worktree ") {
            current.path = path.to_string();
        } else if let Some(branch_ref) = line.strip_prefix("branch ") {
            // Strip "refs/heads/" prefix
            current.branch = branch_ref
                .strip_prefix("refs/heads/")
                .or_else(|| branch_ref.strip_prefix("refs/remotes/"))
                .map(|s| s.to_string())
                .or_else(|| Some(branch_ref.to_string()));
        } else if let Some(sha) = line.strip_prefix("HEAD ") {
            current.head_sha = Some(sha.to_string());
        } else if line == "bare" {
            current.bare = true;
        }
    }

    // Push the last entry
    if in_entry {
        entries.push(current);
    }

    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_worktree_list() {
        let output = "worktree /path/to/worktree1\nHEAD abc123def\nbranch refs/heads/feature/foo\n\nworktree /path/to/worktree2\nHEAD def456ghi\nbranch refs/heads/main\nbare\n";
        let entries = parse_worktree_list(output);
        assert_eq!(entries.len(), 2);

        assert_eq!(entries[0].path, "/path/to/worktree1");
        assert_eq!(entries[0].head_sha.as_deref(), Some("abc123def"));
        assert_eq!(entries[0].branch.as_deref(), Some("feature/foo"));
        assert!(!entries[0].bare);

        assert_eq!(entries[1].path, "/path/to/worktree2");
        assert_eq!(entries[1].head_sha.as_deref(), Some("def456ghi"));
        assert_eq!(entries[1].branch.as_deref(), Some("main"));
        assert!(entries[1].bare);
    }

    #[test]
    fn test_parse_worktree_list_detached() {
        let output = "worktree /path/to/wt\nHEAD abc123\n";
        let entries = parse_worktree_list(output);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "/path/to/wt");
        assert!(entries[0].branch.is_none());
        assert_eq!(entries[0].head_sha.as_deref(), Some("abc123"));
    }

    #[test]
    fn test_parse_worktree_list_empty() {
        let entries = parse_worktree_list("");
        assert!(entries.is_empty());
    }
}
