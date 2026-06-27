use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;

/// Snapshot of a git repository at a point in time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoSnapshot {
    /// HEAD commit SHA.
    pub head: String,
    /// Output of `git status --porcelain=v1 --untracked-files=all`.
    pub status_porcelain: String,
    /// Output of `git write-tree`.
    pub index_tree: String,
    /// Current branch name.
    pub current_branch: String,
    /// Output of `git worktree list --porcelain`.
    pub worktree_list_text: String,
}

/// A seeded (pre-initialised) git repository for E2E tests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeededRepo {
    /// Path to the repository.
    pub path: String,
    /// Default branch name.
    pub default_branch: String,
    /// SHA of the initial commit.
    pub initial_commit: String,
}

/// Create a seeded repository at a temporary location.
///
/// Initialises a bare repo, creates an initial commit with `README.md`,
/// and returns the [`SeededRepo`] handle.
///
/// # Panics
/// Panics if any git operation fails.
pub fn create_seeded_repo(git_path: &str) -> SeededRepo {
    let repo_path = std::env::temp_dir().join(format!("seeded-repo-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&repo_path).expect("create seeded repo dir");

    run_git(git_path, &repo_path, &["init", "-b", "main"]);
    run_git(git_path, &repo_path, &["config", "user.name", "Looper E2E"]);
    run_git(git_path, &repo_path, &["config", "user.email", "looper-e2e@example.com"]);
    run_git(git_path, &repo_path, &["config", "commit.gpgsign", "false"]);

    std::fs::write(repo_path.join("README.md"), b"# looper e2e\n").expect("write README");
    run_git(git_path, &repo_path, &["add", "README.md"]);
    run_git(git_path, &repo_path, &["commit", "-m", "initial commit"]);

    let initial_commit = run_git(git_path, &repo_path, &["rev-parse", "HEAD"]).trim().to_string();

    SeededRepo { path: repo_path.to_string_lossy().to_string(), default_branch: "main".to_string(), initial_commit }
}

/// Snapshot the current state of the repository at `repo_path`.
///
/// # Panics
/// Panics if any git operation fails.
pub fn snapshot_repo(git_path: &str, repo_path: impl AsRef<Path>) -> RepoSnapshot {
    let repo = repo_path.as_ref();
    let head = run_git(git_path, repo, &["rev-parse", "HEAD"]).trim().to_string();
    let status_porcelain = run_git(git_path, repo, &["status", "--porcelain=v1", "--untracked-files=all"]);
    let index_tree = run_git(git_path, repo, &["write-tree"]).trim().to_string();
    let current_branch = run_git(git_path, repo, &["branch", "--show-current"]).trim().to_string();
    let worktree_list_text = run_git(git_path, repo, &["worktree", "list", "--porcelain"]);

    RepoSnapshot { head, status_porcelain, index_tree, current_branch, worktree_list_text }
}

/// Run a git command and return its stdout.
///
/// # Panics
/// Panics if the command fails or the git binary is not found.
pub fn run_git(git_path: &str, cwd: &Path, args: &[&str]) -> String {
    let output = Command::new(git_path)
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap_or_else(|e| panic!("failed to execute git {}: {}", args.join(" "), e));

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        panic!(
            "git {} failed:\nstdout: {}\nstderr: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            stderr
        );
    }

    String::from_utf8_lossy(&output.stdout).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_seeded_repo() {
        // Use the system git.
        let repo = create_seeded_repo("git");
        assert!(!repo.initial_commit.is_empty());
        assert_eq!(repo.default_branch, "main");
        // Cleanup.
        let _ = std::fs::remove_dir_all(&repo.path);
    }

    #[test]
    fn test_snapshot_repo() {
        let repo = create_seeded_repo("git");
        let snap = snapshot_repo("git", &repo.path);
        assert_eq!(snap.head, repo.initial_commit);
        assert_eq!(snap.current_branch, "main");
        let _ = std::fs::remove_dir_all(&repo.path);
    }
}
