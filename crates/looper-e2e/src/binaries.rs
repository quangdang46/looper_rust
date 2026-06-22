use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Environment variable names
// ---------------------------------------------------------------------------

const ENV_LOOPER_PATH: &str = "LOOPER_E2E_LOOPER_PATH";
const ENV_LOOPERD_PATH: &str = "LOOPER_E2E_LOOPERD_PATH";
const ENV_FAKE_AGENT_PATH: &str = "LOOPER_E2E_FAKE_AGENT_PATH";
const ENV_FAKE_GH_PATH: &str = "LOOPER_E2E_FAKE_GH_PATH";
const ENV_FAKE_OSASCRIPT_PATH: &str = "LOOPER_E2E_FAKE_OSASCRIPT_PATH";

/// Paths to all built binaries required by an E2E test.
///
/// Populated by reading the `LOOPER_E2E_*` environment variables, or by
/// building all binaries (future work).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuiltBinaries {
    /// Path to the `looper` CLI binary.
    pub looper_path: PathBuf,
    /// Path to the `looperd` daemon binary.
    pub looperd_path: PathBuf,
    /// Path to the `fake-agent` binary.
    pub fake_agent_path: PathBuf,
    /// Path to the `fake-gh` binary.
    pub fake_gh_path: PathBuf,
    /// Path to the `fake-osascript` binary.
    pub fake_osascript_path: PathBuf,
}

/// Return the [`BuiltBinaries`] from environment variables, or panic.
///
/// Required env vars:
/// - `LOOPER_E2E_LOOPER_PATH`
/// - `LOOPER_E2E_LOOPERD_PATH`
/// - `LOOPER_E2E_FAKE_AGENT_PATH`
/// - `LOOPER_E2E_FAKE_GH_PATH`
/// - `LOOPER_E2E_FAKE_OSASCRIPT_PATH`
///
/// # Panics
/// Panics if any env var is missing or empty.
pub fn must_binaries() -> BuiltBinaries {
    let read = |name: &str| -> PathBuf {
        std::env::var(name)
            .unwrap_or_else(|_| panic!("{} must be set", name))
            .into()
    };

    BuiltBinaries {
        looper_path: read(ENV_LOOPER_PATH),
        looperd_path: read(ENV_LOOPERD_PATH),
        fake_agent_path: read(ENV_FAKE_AGENT_PATH),
        fake_gh_path: read(ENV_FAKE_GH_PATH),
        fake_osascript_path: read(ENV_FAKE_OSASCRIPT_PATH),
    }
}

impl BuiltBinaries {
    /// Try to read all five paths from environment variables.
    ///
    /// Returns `Some` only when every variable is set and non-empty.
    pub fn from_env() -> Option<Self> {
        let looper_path = std::env::var(ENV_LOOPER_PATH).ok()?;
        let looperd_path = std::env::var(ENV_LOOPERD_PATH).ok()?;
        let fake_agent_path = std::env::var(ENV_FAKE_AGENT_PATH).ok()?;
        let fake_gh_path = std::env::var(ENV_FAKE_GH_PATH).ok()?;
        let fake_osascript_path = std::env::var(ENV_FAKE_OSASCRIPT_PATH).ok()?;

        if looper_path.is_empty()
            || looperd_path.is_empty()
            || fake_agent_path.is_empty()
            || fake_gh_path.is_empty()
            || fake_osascript_path.is_empty()
        {
            return None;
        }

        Some(Self {
            looper_path: PathBuf::from(looper_path),
            looperd_path: PathBuf::from(looperd_path),
            fake_agent_path: PathBuf::from(fake_agent_path),
            fake_gh_path: PathBuf::from(fake_gh_path),
            fake_osascript_path: PathBuf::from(fake_osascript_path),
        })
    }

    /// Set the five `LOOPER_E2E_*` environment variables to the paths in
    /// this struct.
    pub fn set_env(&self) {
        std::env::set_var(ENV_LOOPER_PATH, &self.looper_path);
        std::env::set_var(ENV_LOOPERD_PATH, &self.looperd_path);
        std::env::set_var(ENV_FAKE_AGENT_PATH, &self.fake_agent_path);
        std::env::set_var(ENV_FAKE_GH_PATH, &self.fake_gh_path);
        std::env::set_var(ENV_FAKE_OSASCRIPT_PATH, &self.fake_osascript_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_env_missing_returns_none() {
        // Unset all so from_env returns None.
        std::env::remove_var(ENV_LOOPER_PATH);
        std::env::remove_var(ENV_LOOPERD_PATH);
        std::env::remove_var(ENV_FAKE_AGENT_PATH);
        std::env::remove_var(ENV_FAKE_GH_PATH);
        std::env::remove_var(ENV_FAKE_OSASCRIPT_PATH);
        assert!(BuiltBinaries::from_env().is_none());
    }
}
