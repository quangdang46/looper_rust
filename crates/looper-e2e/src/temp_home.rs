use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Temporary home directory and all sub-directories used in an E2E test.
///
/// Created via [`TempHome::new`] which allocates a unique temporary root
/// and populates every standard path beneath it.  The caller should set
/// `HOME` (and/or `LOOPER_HOME`) to `self.home_dir` before launching any
/// looper process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TempHome {
    /// Root of the temporary sandbox.  All other paths live under this.
    pub root: PathBuf,
    /// Synthetic `$HOME` directory.
    pub home_dir: PathBuf,
    /// `$HOME/.looper` directory.
    pub looper_home: PathBuf,
    /// Test-artifact directory (not tied to a specific test name).
    pub artifacts_dir: PathBuf,
    /// Log directory (`$LOOPER_HOME/logs`).
    pub log_dir: PathBuf,
    /// Backup directory (`$LOOPER_HOME/backups`).
    pub backup_dir: PathBuf,
    /// Worktree root (`$LOOPER_HOME/worktrees`).
    pub worktree_root: PathBuf,
    /// Working directory for the looper daemon / tests.
    pub working_dir: PathBuf,
    /// SQLite database path.
    pub db_path: PathBuf,
    /// Config file path.
    pub config_path: PathBuf,
}

impl TempHome {
    /// Create a new temporary sandbox with all standard directories.
    ///
    /// Creates a unique temp directory via `std::env::temp_dir()`, then
    /// builds every internal path and creates the directories on disk.
    /// Panics if any `mkdir` call fails.
    pub fn new(prefix: &str) -> Self {
        let root = TempHome::create_temp_root(prefix);
        let home_dir = root.join("home");
        let looper_home = home_dir.join(".looper");
        let artifacts_dir = root.join("artifacts");
        let log_dir = looper_home.join("logs");
        let backup_dir = looper_home.join("backups");
        let worktree_root = looper_home.join("worktrees");
        let working_dir = root.join("working");

        for dir in [
            &home_dir,
            &looper_home,
            &artifacts_dir,
            &log_dir,
            &backup_dir,
            &worktree_root,
            &working_dir,
        ] {
            std::fs::create_dir_all(dir).expect("failed to create temp home directory");
        }

        let db_path = looper_home.join("looper.sqlite");
        let config_path = looper_home.join("config.json");

        Self {
            root,
            home_dir,
            looper_home,
            artifacts_dir,
            log_dir,
            backup_dir,
            worktree_root,
            working_dir,
            db_path,
            config_path,
        }
    }

    /// Build an environment map with `HOME` set to `self.home_dir`.
    pub fn env_map(&self) -> std::collections::HashMap<String, String> {
        let mut m = std::collections::HashMap::new();
        m.insert("HOME".to_string(), self.home_dir.to_string_lossy().to_string());
        m
    }

    /// Build an environment-variable slice suitable for spawning child processes.
    pub fn env_slice(&self) -> Vec<String> {
        vec![format!("HOME={}", self.home_dir.to_string_lossy())]
    }

    // -----------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------

    fn create_temp_root(prefix: &str) -> PathBuf {
        let base = std::env::temp_dir();
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir_name = format!("{}-{}", prefix, ts);
        let path = base.join(&dir_name);
        std::fs::create_dir_all(&path).expect("failed to create temp root");
        path
    }

    /// Remove the entire sandbox directory tree.
    pub fn cleanup(&self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
