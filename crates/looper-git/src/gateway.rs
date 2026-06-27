use std::sync::Arc;

use crate::error::{GitError, Result};
use crate::helpers;
use crate::safety::{self, SafetyCheckInput};
use crate::types::*;
use looper_storage::WorktreeRecord;

/// Gateway wraps git CLI operations for worktree management.
///
/// Mirrors the Go implementation's `shell.Run` pattern — all git commands
/// are executed via `tokio::process::Command`.
pub struct Gateway {
    /// Path to the git binary (defaults to "git").
    pub git_path: String,
    /// Optional storage for DB persistence of worktree records.
    pub repos: Option<Arc<looper_storage::repos::Repositories>>,
    /// Clock function.
    pub now: fn() -> chrono::DateTime<chrono::Utc>,
}

impl Gateway {
    /// Create a new Gateway with the given options.
    pub fn new(options: GatewayOptions) -> Self {
        Self { git_path: options.git_path, repos: options.repos, now: options.now }
    }

    // -----------------------------------------------------------------------
    // 1. CreateWorktree
    // -----------------------------------------------------------------------

    /// Create a new worktree, or restore an existing one from DB if possible.
    pub async fn create_worktree(&self, input: CreateWorktreeInput) -> Result<CreateWorktreeResult> {
        // 1. Validate branch is writable
        safety::assert_writable_branch(&input.branch, &input.protected_branches)?;

        // 2. Create worktree root directory
        tokio::fs::create_dir_all(&input.worktree_root).await?;

        // 3. Compute worktree path
        let dir_name = build_worktree_directory_name(&input);
        let worktree_path = std::path::Path::new(&input.worktree_root).join(&dir_name);
        let worktree_path_str = worktree_path.to_string_lossy().to_string();

        // 4. Validate path safety
        let safety_input = SafetyCheckInput {
            path: worktree_path_str.clone(),
            repo_path: Some(input.repo_path.clone()),
            worktree_root: Some(input.worktree_root.clone()),
        };
        safety::validate_worktree_path(&safety_input)?;

        // 5. Attempt restore from DB first
        if let Some(ref repos) = self.repos {
            if let Ok(Some(record)) = repos.worktrees.get_by_branch(&input.project_id, &input.branch) {
                if record.status != "cleaned"
                    && record.repo_path == input.repo_path
                    && self.is_healthy_worktree(&record.worktree_path).await?
                {
                    let mut updated = record.clone();
                    updated.head_sha = helpers::get_head_sha(&record.worktree_path).await.ok();
                    updated.updated_at = (self.now)().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
                    repos.worktrees.upsert(&updated)?;
                    return Ok(CreateWorktreeResult { record: updated, recovered: true });
                }
            }
        }

        // 6. Create the git worktree
        let start_point = input.start_point.as_deref().unwrap_or(&input.branch);
        if input.checkout_mode == CheckoutMode::Detached {
            helpers::run_git_cmd(
                &input.repo_path,
                ["worktree", "add", "--force", "--detach", &worktree_path_str, start_point],
            )
            .await?;
        } else if local_branch_exists_in_path(&input.repo_path, &input.branch).await? {
            helpers::run_git_cmd(&input.repo_path, ["worktree", "add", "--force", &worktree_path_str, &input.branch])
                .await?;
        } else {
            helpers::run_git_cmd(
                &input.repo_path,
                ["worktree", "add", "--force", "-b", &input.branch, &worktree_path_str, start_point],
            )
            .await?;
        }

        // 7. Get HEAD SHA
        let head_sha = helpers::get_head_sha(&worktree_path_str).await?;

        // 8. Upsert worktree record in DB
        let now_str = (self.now)().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
        let record = WorktreeRecord {
            id: uuid::Uuid::new_v4().to_string(),
            project_id: input.project_id.clone(),
            repo_path: input.repo_path.clone(),
            worktree_path: worktree_path_str.clone(),
            branch: input.branch.clone(),
            base_branch: input.base_branch.clone(),
            status: "active".to_string(),
            head_sha: Some(head_sha.clone()),
            metadata_json: None,
            created_at: now_str.clone(),
            updated_at: now_str,
            cleaned_at: None,
        };

        if let Some(ref repos) = self.repos {
            repos.worktrees.upsert(&record)?;
        }

        Ok(CreateWorktreeResult { record, recovered: false })
    }

    // -----------------------------------------------------------------------
    // 2. RestoreWorktree
    // -----------------------------------------------------------------------

    /// Restore an existing worktree from DB — called during daemon startup.
    pub async fn restore_worktree(&self, input: RestoreWorktreeInput) -> Result<Option<WorktreeRecord>> {
        // Look up existing DB record
        let repos = match self.repos {
            Some(ref r) => r,
            None => return Ok(None),
        };

        let record = match repos.worktrees.get_by_branch(&input.project_id, &input.branch)? {
            Some(r) if r.status != "cleaned" && r.repo_path == input.repo_path => r,
            _ => {
                // Check git worktree list for existing worktree
                let entries = self.list_worktrees_internal(&input.repo_path).await?;
                for entry in &entries {
                    if self.matches_checkout_mode(entry, &input.checkout_mode, &input.branch)
                        && self.is_healthy_worktree(&entry.path).await?
                    {
                        let now_str = (self.now)().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
                        let head_sha = helpers::get_head_sha(&entry.path).await.ok();
                        let new_record = WorktreeRecord {
                            id: uuid::Uuid::new_v4().to_string(),
                            project_id: input.project_id.clone(),
                            repo_path: input.repo_path.clone(),
                            worktree_path: entry.path.clone(),
                            branch: input.branch.clone(),
                            base_branch: input.base_branch.clone(),
                            status: "active".to_string(),
                            head_sha,
                            metadata_json: Some("{\"recovered\":true}".to_string()),
                            created_at: now_str.clone(),
                            updated_at: now_str,
                            cleaned_at: None,
                        };
                        repos.worktrees.upsert(&new_record)?;
                        return Ok(Some(new_record));
                    }
                }
                return Ok(None);
            }
        };

        // Validate safety
        let safety_input = SafetyCheckInput {
            path: record.worktree_path.clone(),
            repo_path: Some(input.repo_path.clone()),
            worktree_root: None,
        };
        safety::validate_worktree_path(&safety_input)?;

        // Check health
        if !self.is_healthy_worktree(&record.worktree_path).await? {
            return Ok(None);
        }

        // Verify checkout mode matches
        if !self.matches_checkout_mode_for_path(&record.worktree_path, &input.checkout_mode, &input.branch).await? {
            return Ok(None);
        }

        // Update record
        let now_str = (self.now)().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
        let mut updated = record.clone();
        updated.head_sha = helpers::get_head_sha(&record.worktree_path).await.ok();
        updated.status = "active".to_string();
        updated.updated_at = now_str;
        repos.worktrees.upsert(&updated)?;

        Ok(Some(updated))
    }

    // -----------------------------------------------------------------------
    // 3. CleanupWorktree
    // -----------------------------------------------------------------------

    /// Remove a worktree and mark it "cleaned" in the DB.
    pub async fn cleanup_worktree(&self, input: CleanupWorktreeInput) -> Result<()> {
        // 1. Validate branch is writable
        safety::assert_writable_branch(&input.branch, &input.protected_branches)?;

        // 2. Validate path safety
        let safety_input = SafetyCheckInput {
            path: input.worktree_path.clone(),
            repo_path: Some(input.repo_path.clone()),
            worktree_root: None,
        };
        safety::validate_worktree_path(&safety_input)?;

        // 3. Run git worktree remove
        let result =
            helpers::run_git_cmd(&input.repo_path, ["worktree", "remove", "--force", &input.worktree_path]).await;

        // 4. If error matches missing worktree pattern, ignore
        if let Err(ref e) = result {
            if let GitError::CommandError { ref stderr, .. } = e {
                if crate::error::is_missing_worktree_error(stderr) {
                    // Worktree already gone — continue
                } else {
                    return Err(GitError::CommandError {
                        exit_code: match e {
                            GitError::CommandError { exit_code, .. } => *exit_code,
                            _ => -1,
                        },
                        stderr: stderr.clone(),
                    });
                }
            } else {
                return Err(result.unwrap_err());
            }
        }

        // 5. Update DB record
        if let Some(ref repos) = self.repos {
            if let Ok(Some(existing)) = repos.worktrees.get_by_branch(&input.project_id(), &input.branch) {
                let now_str = (self.now)().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
                let mut updated = existing;
                updated.status = "cleaned".to_string();
                updated.cleaned_at = Some(now_str.clone());
                updated.updated_at = now_str;
                repos.worktrees.upsert(&updated)?;
            }
        }

        Ok(())
    }

    // -----------------------------------------------------------------------
    // 4. ListWorktrees
    // -----------------------------------------------------------------------

    /// List worktrees in a repository via `git worktree list --porcelain`.
    pub async fn list_worktrees(&self, repo_path: &str) -> Result<Vec<WorktreeListEntry>> {
        self.list_worktrees_internal(repo_path).await
    }

    async fn list_worktrees_internal(&self, repo_path: &str) -> Result<Vec<WorktreeListEntry>> {
        let output = helpers::run_git_cmd(repo_path, ["worktree", "list", "--porcelain"]).await?;
        Ok(helpers::parse_worktree_list(&output))
    }

    // -----------------------------------------------------------------------
    // 5. WorktreeClean / IsWorktreeClean
    // -----------------------------------------------------------------------

    /// Check if a worktree has uncommitted changes.
    pub async fn worktree_clean(&self, worktree_path: &str) -> Result<bool> {
        let output = helpers::run_git_cmd(worktree_path, ["status", "--porcelain", "--untracked-files=all"]).await?;
        Ok(output.trim().is_empty())
    }

    /// Alias for worktree_clean.
    pub async fn is_worktree_clean(&self, worktree_path: &str) -> Result<bool> {
        self.worktree_clean(worktree_path).await
    }

    // -----------------------------------------------------------------------
    // 6. PrepareWorktree
    // -----------------------------------------------------------------------

    /// Fetch + reset worktree to match remote.
    pub async fn prepare_worktree(&self, input: PrepareWorktreeInput) -> Result<PrepareWorktreeResult> {
        // Validate path safety
        let safety_input = SafetyCheckInput { path: input.worktree_path.clone(), repo_path: None, worktree_root: None };
        safety::validate_worktree_path(&safety_input)?;

        // Check if clean before reset
        let is_clean = self.worktree_clean(&input.worktree_path).await?;

        // Fetch
        let remote = input.remote.as_deref().unwrap_or("origin");
        helpers::run_git_with_retry(&input.worktree_path, ["fetch", remote, &input.target_spec]).await?;

        // Reset if clean and local != remote
        if is_clean {
            let _ = helpers::run_git_cmd(&input.worktree_path, ["reset", "--hard", &input.reset_ref]).await;
        }

        let head_sha = helpers::get_head_sha(&input.worktree_path).await?;

        Ok(PrepareWorktreeResult { head_sha, was_dirty: !is_clean })
    }

    // -----------------------------------------------------------------------
    // 7. InspectHead
    // -----------------------------------------------------------------------

    /// Get HEAD SHA, new commits since base, and changed files.
    pub async fn inspect_head(&self, input: InspectHeadInput) -> Result<InspectHeadResult> {
        let head_sha = helpers::get_head_sha(&input.worktree_path).await?;

        let new_commits = if let Some(ref base) = input.base_ref {
            let output =
                helpers::run_git_cmd(&input.worktree_path, ["rev-list", "--reverse", &format!("{}..HEAD", base)])
                    .await?;
            output.lines().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
        } else {
            Vec::new()
        };

        let output = helpers::run_git_cmd(
            &input.worktree_path,
            ["status", "--porcelain", "--untracked-files=all", "--ignored=no"],
        )
        .await?;
        let changed_files: Vec<String> = output
            .lines()
            .map(|s| {
                // Strip first 3 chars (status markers + space) to get filename
                let trimmed = s.trim();
                if trimmed.len() > 3 {
                    trimmed[3..].to_string()
                } else {
                    trimmed.to_string()
                }
            })
            .filter(|s| !s.is_empty())
            .collect();

        Ok(InspectHeadResult { head_sha, new_commits, changed_files })
    }

    // -----------------------------------------------------------------------
    // 8. Commit
    // -----------------------------------------------------------------------

    /// Stage all changes and commit in the worktree.
    pub async fn commit(&self, input: CommitInput) -> Result<CommitResult> {
        helpers::run_git_cmd(&input.worktree_path, ["add", "-A"]).await?;
        helpers::run_git_cmd(&input.worktree_path, ["commit", "--allow-empty", "-m", &input.message]).await?;

        let head_sha = helpers::get_head_sha(&input.worktree_path).await?;
        Ok(CommitResult { head_sha })
    }

    // -----------------------------------------------------------------------
    // 9. Push
    // -----------------------------------------------------------------------

    /// Push changes with `--force-with-lease` or simple `-u`.
    pub async fn push(&self, input: PushInput) -> Result<()> {
        // Validate branch is writable
        safety::assert_writable_branch(&input.branch, &input.protected_branches)?;

        let refspec = format!("HEAD:refs/heads/{}", input.branch);

        if let Some(ref expected_sha) = input.expected_head_sha {
            // Verify local HEAD descends from expected SHA
            let is_ancestor = self.is_ancestor_internal(&input.worktree_path, expected_sha, "HEAD").await?;

            if !is_ancestor {
                // Get actual remote head for error
                let actual_sha =
                    helpers::run_git_cmd(&input.worktree_path, ["ls-remote", "--heads", &input.remote, &input.branch])
                        .await?;
                let actual = actual_sha.split_whitespace().next().unwrap_or("unknown").to_string();

                return Err(GitError::RemoteHeadChanged(crate::error::RemoteHeadChangedError {
                    branch: input.branch.clone(),
                    expected_head_sha: expected_sha.clone(),
                    actual_head_sha: actual,
                }));
            }

            let lease = format!("refs/heads/{}:{}", input.branch, expected_sha);
            helpers::run_git_cmd(
                &input.worktree_path,
                ["push", "--porcelain", &format!("--force-with-lease={}", &lease), "-u", &input.remote, &refspec],
            )
            .await?;
        } else {
            helpers::run_git_cmd(&input.worktree_path, ["push", "-u", &input.remote, &refspec]).await?;
        }

        Ok(())
    }

    // -----------------------------------------------------------------------
    // 10. CreateBranch
    // -----------------------------------------------------------------------

    /// Create a branch (or force-reset an existing one).
    pub async fn create_branch(
        &self,
        repo_path: &str,
        branch: &str,
        start_point: &str,
        protected_branches: &[String],
    ) -> Result<()> {
        safety::assert_writable_branch(branch, protected_branches)?;

        helpers::run_git_cmd(repo_path, ["branch", "--force", branch, start_point]).await?;

        Ok(())
    }

    // -----------------------------------------------------------------------
    // 11. DetectGitHubRepo
    // -----------------------------------------------------------------------

    /// Parse the remote origin URL to extract the GitHub owner/repo.
    pub async fn detect_github_repo(&self, repo_path: &str) -> Result<String> {
        let output = helpers::run_git_cmd(repo_path, ["config", "--get", "remote.origin.url"]).await?;

        let url = output.trim();
        parse_github_repo(url)
            .ok_or_else(|| GitError::Other(format!("could not parse GitHub repo from remote URL: {}", url)))
    }

    // -----------------------------------------------------------------------
    // 12. FetchBranch
    // -----------------------------------------------------------------------

    /// Fetch a specific branch from a remote.
    pub async fn fetch_branch(&self, repo_path: &str, remote: &str, branch: &str) -> Result<()> {
        helpers::run_git_with_retry(repo_path, ["fetch", remote, branch]).await?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // 13. IsAncestor
    // -----------------------------------------------------------------------

    /// Check if `ancestor` is an ancestor of `descendant`.
    pub async fn is_ancestor(&self, repo_path: &str, ancestor: &str, descendant: &str) -> Result<bool> {
        self.is_ancestor_internal(repo_path, ancestor, descendant).await
    }

    async fn is_ancestor_internal(&self, repo_path: &str, ancestor: &str, descendant: &str) -> Result<bool> {
        let output = tokio::process::Command::new(&self.git_path)
            .args(["merge-base", "--is-ancestor", ancestor, descendant])
            .current_dir(repo_path)
            .output()
            .await?;

        Ok(output.status.success())
    }

    // -----------------------------------------------------------------------
    // 14. AssertWritableBranch (delegates to safety module)
    // -----------------------------------------------------------------------

    pub fn assert_writable_branch(&self, branch: &str, protected_branches: &[String]) -> Result<()> {
        safety::assert_writable_branch(branch, protected_branches)
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Check if a worktree is healthy (exists on disk and git status succeeds).
    async fn is_healthy_worktree(&self, worktree_path: &str) -> Result<bool> {
        let path = std::path::Path::new(worktree_path);
        if !path.exists() {
            return Ok(false);
        }

        // Try running git status to verify it's a valid git worktree
        match helpers::run_git_cmd(worktree_path, ["status", "--porcelain"]).await {
            Ok(_) => Ok(true),
            Err(_) => Ok(false),
        }
    }

    /// Check if a parsed WorktreeListEntry matches the expected checkout mode and branch.
    fn matches_checkout_mode(&self, entry: &WorktreeListEntry, mode: &CheckoutMode, branch: &str) -> bool {
        match mode {
            CheckoutMode::Detached => entry.branch.is_none(),
            CheckoutMode::Branch => entry.branch.as_deref() == Some(branch),
        }
    }

    /// Check checkout mode by running git commands on the worktree path.
    async fn matches_checkout_mode_for_path(&self, path: &str, mode: &CheckoutMode, branch: &str) -> Result<bool> {
        match mode {
            CheckoutMode::Detached => Ok(helpers::is_detached(path).await?),
            CheckoutMode::Branch => {
                let output = helpers::run_git_cmd(path, ["rev-parse", "--abbrev-ref", "HEAD"]).await?;
                Ok(output.trim() == branch)
            }
        }
    }
}

// -----------------------------------------------------------------------
// Free functions
// -----------------------------------------------------------------------

/// Parse a GitHub remote URL to extract "owner/repo".
pub fn parse_github_repo(url: &str) -> Option<String> {
    let url = url.trim();

    // SSH: git@github.com:owner/repo.git
    if let Some(rest) = url.strip_prefix("git@github.com:") {
        return rest.strip_suffix(".git").or(Some(rest)).map(|s| s.to_string());
    }

    // HTTPS: https://github.com/owner/repo.git
    if let Some(rest) = url.strip_prefix("https://github.com/") {
        return rest.strip_suffix(".git").or(Some(rest)).map(|s| s.to_string());
    }

    None
}

/// Check if a local branch exists at a given path (free function for use during create).
async fn local_branch_exists_in_path(repo_path: &str, branch: &str) -> Result<bool> {
    let output = tokio::process::Command::new("git")
        .args(["show-ref", "--quiet", "--verify", &format!("refs/heads/{}", branch)])
        .current_dir(repo_path)
        .output()
        .await?;
    Ok(output.status.success())
}

// -----------------------------------------------------------------------
// Helper to extract project_id from CleanupWorktreeInput.
// The input doesn't have project_id directly, so we provide a helper
// that can be called when DB is available.
// -----------------------------------------------------------------------

impl CleanupWorktreeInput {
    /// Get the project_id from an associated WorktreeRecord if available via repos.
    /// This is a placeholder — callers should set project_id when constructing the input.
    pub fn project_id(&self) -> String {
        // Derive from repo_path basename as fallback
        std::path::Path::new(&self.repo_path).file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_github_repo_ssh() {
        assert_eq!(parse_github_repo("git@github.com:owner/repo.git"), Some("owner/repo".to_string()));
    }

    #[test]
    fn test_parse_github_repo_https() {
        assert_eq!(parse_github_repo("https://github.com/owner/repo.git"), Some("owner/repo".to_string()));
    }

    #[test]
    fn test_parse_github_repo_no_git_suffix() {
        assert_eq!(parse_github_repo("git@github.com:owner/repo"), Some("owner/repo".to_string()));
    }

    #[test]
    fn test_parse_github_repo_non_github() {
        assert_eq!(parse_github_repo("git@gitlab.com:owner/repo.git"), None);
    }

    #[test]
    fn test_parse_github_repo_empty() {
        assert_eq!(parse_github_repo(""), None);
    }
}
