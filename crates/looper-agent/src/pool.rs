//! Looper worktree pool — wraps treehouse-core for worktree management.
//!
//! Provides bounded worktree pools with lease TTL, PID-reuse-safe tracking,
//! and automatic garbage collection. Replaces ad-hoc worktree creation with
//! a managed pool that prevents orphan worktrees and process leaks.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use treehouse_core::config::TreehouseConfig;
use treehouse_core::env::{FileMeta, TreehouseEnv};
use treehouse_core::pool::{AcquireOptions, LeaseAcquireOptions, PoolError, WorktreeStatus};

/// Configuration for the looper worktree pool.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct PoolConfig {
    /// Maximum worktrees per project (default: 16).
    pub max_trees: u32,
    /// File lock timeout for pool state (default: 10s).
    pub lock_timeout_secs: u64,
    /// Background GC interval (default: 300s = 5 min).
    pub gc_interval_secs: u64,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self { max_trees: 16, lock_timeout_secs: 10, gc_interval_secs: 300 }
    }
}

/// Looper-specific environment that implements treehouse-core's `TreehouseEnv`.
///
/// Pool root is `.looper/worktrees/` under the project repo. No config files,
/// no update cache — all configuration comes from looper's own config system.
pub struct LooperEnv {
    base: PathBuf,
}

impl LooperEnv {
    /// Create a new LooperEnv rooted at the given base directory.
    pub fn new(base: PathBuf) -> Self {
        Self { base }
    }
}

impl TreehouseEnv for LooperEnv {
    fn pool_root(&self) -> Option<PathBuf> {
        Some(self.base.join("worktrees"))
    }

    fn update_cache_path(&self) -> Option<PathBuf> {
        None
    }

    fn user_config_path(&self) -> Option<PathBuf> {
        None
    }

    fn read_file(&self, path: &Path) -> io::Result<String> {
        std::fs::read_to_string(path)
    }

    fn read_bytes(&self, path: &Path) -> io::Result<Vec<u8>> {
        std::fs::read(path)
    }

    fn write_file(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, data)
    }

    fn ensure_dir(&self, path: &Path) -> io::Result<()> {
        std::fs::create_dir_all(path)
    }

    fn path_exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn list_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        std::fs::read_dir(path).map(|entries| entries.filter_map(|e| e.ok()).map(|e| e.path()).collect())
    }

    fn file_meta(&self, path: &Path) -> io::Result<FileMeta> {
        let meta = std::fs::metadata(path)?;
        Ok(FileMeta { size: meta.len(), modified: meta.modified().ok() })
    }

    fn env_var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }

    fn env_var_os(&self, name: &str) -> Option<PathBuf> {
        std::env::var_os(name).map(PathBuf::from)
    }

    fn cwd(&self) -> Option<PathBuf> {
        std::env::current_dir().ok()
    }
}

/// Result of acquiring a worktree from the pool.
pub struct AcquiredWorktree {
    /// Name/ID of the worktree in the pool.
    pub name: String,
    /// Absolute path to the worktree directory.
    pub path: PathBuf,
    /// Branch the worktree is on.
    pub branch: String,
    /// Lease ID if leased (non-interactive mode).
    pub lease_id: Option<String>,
}

/// Looper worktree pool wrapping treehouse-core.
pub struct LooperPool {
    core: treehouse_core::TreehouseCore<LooperEnv>,
    repo_root: PathBuf,
    remote_url: Option<String>,
}

impl LooperPool {
    /// Create a new pool for a project repository.
    pub fn new(repo_root: &Path, remote_url: Option<String>, config: PoolConfig) -> Self {
        let env = LooperEnv::new(repo_root.to_path_buf());
        let th_config = TreehouseConfig { max_trees: config.max_trees, ..TreehouseConfig::default_config() };
        let core = treehouse_core::TreehouseCore::with_env(env, th_config);

        Self { core, repo_root: repo_root.to_path_buf(), remote_url }
    }

    /// Acquire a worktree for an agent to use.
    ///
    /// The worktree is leased with a TTL matching the agent's max runtime.
    /// If the pool is full, returns an error.
    pub fn acquire_for_agent(
        &self,
        vendor: &str,
        loop_id: &str,
        timeout: std::time::Duration,
    ) -> Result<AcquiredWorktree, PoolError> {
        let holder = format!("{vendor}:{loop_id}");
        let ttl = chrono::Duration::from_std(timeout).unwrap_or(chrono::Duration::hours(1));

        let pool = self.core.open_pool(&self.repo_root, self.remote_url.as_deref())?;
        let acquired = pool.get(&AcquireOptions {
            lease: Some(LeaseAcquireOptions { holder, ttl: Some(ttl) }),
            ..Default::default()
        })?;

        Ok(AcquiredWorktree {
            name: acquired.name,
            path: acquired.path,
            branch: acquired.branch,
            lease_id: acquired.lease.map(|l| l.id),
        })
    }

    /// Release a worktree back to the pool.
    ///
    /// After the agent exits (normal or killed), the worktree is reset
    /// and made available for the next loop.
    pub fn release(&self, worktree_path: &Path) -> Result<bool, PoolError> {
        let pool = self.core.open_pool(&self.repo_root, self.remote_url.as_deref())?;
        match pool.release(&worktree_path.to_string_lossy()) {
            Ok(()) => Ok(true),
            Err(PoolError::NotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Get status of all worktrees in the pool.
    pub fn status(&self) -> Result<Vec<WorktreeStatus>, PoolError> {
        let pool = self.core.open_pool(&self.repo_root, self.remote_url.as_deref())?;
        pool.status()
    }

    /// Run garbage collection to reclaim expired leases and orphaned worktrees.
    pub fn gc(&self) -> Result<GcResult, PoolError> {
        let pool = self.core.open_pool(&self.repo_root, self.remote_url.as_deref())?;
        let before = pool.status()?.len();

        // Status call triggers heal_state which reclaims expired leases
        let after = pool.status()?.len();

        Ok(GcResult { reclaimed: before.saturating_sub(after), remaining: after })
    }
}

/// Result of a garbage collection run.
#[derive(Debug)]
pub struct GcResult {
    /// Number of worktrees reclaimed.
    pub reclaimed: usize,
    /// Number of worktrees remaining.
    pub remaining: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_config_defaults() {
        let config = PoolConfig::default();
        assert_eq!(config.max_trees, 16);
        assert_eq!(config.lock_timeout_secs, 10);
        assert_eq!(config.gc_interval_secs, 300);
    }

    #[test]
    fn pool_config_serialize_roundtrip() {
        let config = PoolConfig { max_trees: 8, lock_timeout_secs: 5, gc_interval_secs: 60 };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: PoolConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.max_trees, 8);
        assert_eq!(parsed.lock_timeout_secs, 5);
        assert_eq!(parsed.gc_interval_secs, 60);
    }

    #[test]
    fn looper_env_pool_root() {
        let env = LooperEnv::new(PathBuf::from("/project"));
        assert_eq!(env.pool_root(), Some(PathBuf::from("/project/worktrees")));
    }

    #[test]
    fn looper_env_no_config_files() {
        let env = LooperEnv::new(PathBuf::from("/project"));
        assert_eq!(env.update_cache_path(), None);
        assert_eq!(env.user_config_path(), None);
    }

    #[test]
    fn looper_env_file_roundtrip() {
        let env = LooperEnv::new(PathBuf::from("/tmp/looper-test"));
        let path = Path::new("/tmp/looper-test/test.txt");
        env.write_file(path, b"hello world").unwrap();
        assert_eq!(env.read_file(path).unwrap(), "hello world");
        assert!(env.path_exists(path));
        std::fs::remove_dir_all("/tmp/looper-test").ok();
    }
}
