use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

use crate::binaries::BuiltBinaries;

// ---------------------------------------------------------------------------
// Environment variable names (exported for binary use)
// ---------------------------------------------------------------------------

/// Mode env-var for fake-agent ("cwd-evidence", "write-file", "modify-file", etc.).
pub const ENV_FAKE_AGENT_MODE: &str = "LOOPER_E2E_FAKE_AGENT_MODE";
/// Artifact directory.
pub const ENV_FAKE_AGENT_ARTIFACT_DIR: &str = "LOOPER_E2E_FAKE_AGENT_ARTIFACT_DIR";
/// Path to the fake-agent state file.
pub const ENV_FAKE_AGENT_STATE_PATH: &str = "LOOPER_E2E_FAKE_AGENT_STATE_PATH";
/// File to write in "write-file" mode.
pub const ENV_FAKE_AGENT_WRITE_FILE: &str = "LOOPER_E2E_FAKE_AGENT_WRITE_FILE";
/// File to modify in "modify-file" mode.
pub const ENV_FAKE_AGENT_MODIFY_FILE: &str = "LOOPER_E2E_FAKE_AGENT_MODIFY_FILE";
/// Milliseconds to sleep before responding.
pub const ENV_FAKE_AGENT_SLEEP_MS: &str = "LOOPER_E2E_FAKE_AGENT_SLEEP_MS";
/// Path to a git binary.
pub const ENV_FAKE_AGENT_GIT_PATH: &str = "LOOPER_E2E_FAKE_AGENT_GIT_PATH";
/// Path to a gh binary.
pub const ENV_FAKE_AGENT_GH_PATH: &str = "LOOPER_E2E_FAKE_AGENT_GH_PATH";

/// Handle wrapping the `fake-agent` binary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FakeAgent {
    /// Path to the `fake-agent` binary.
    pub path: PathBuf,
    /// Artifact directory for state and evidence.
    pub artifact_dir: PathBuf,
    /// Path to the state JSON file.
    pub state_path: PathBuf,
}

impl FakeAgent {
    /// Create a new [`FakeAgent`] from built binaries.
    ///
    /// Creates the artifact directory tree.
    ///
    /// # Panics
    /// Panics on I/O errors.
    pub fn new(bins: &BuiltBinaries) -> Self {
        let root = std::env::temp_dir().join(format!("fake-agent-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("create fake-agent root");
        let state_path = root.join("state.json");
        Self {
            path: bins.fake_agent_path.clone(),
            artifact_dir: root,
            state_path,
        }
    }

    /// Build the triple (vendor, command, env_map) for injecting into a
    /// looper config's agent section.
    ///
    /// The vendor is hard-coded to `Claude` (matching the Go port's
    /// `config.AgentVendorCodex` in spirit — any vendor works because the
    /// fake agent is a stand-in).
    pub fn agent_config(
        &self,
        mode: &str,
        git_path: &str,
        gh_path: &str,
    ) -> (looper_types::AgentVendor, String, HashMap<String, String>) {
        let vendor = looper_types::AgentVendor::Claude;
        let mut env = HashMap::new();
        env.insert(ENV_FAKE_AGENT_MODE.to_string(), mode.to_string());
        env.insert(
            ENV_FAKE_AGENT_ARTIFACT_DIR.to_string(),
            self.artifact_dir.to_string_lossy().to_string(),
        );
        env.insert(
            ENV_FAKE_AGENT_STATE_PATH.to_string(),
            self.state_path.to_string_lossy().to_string(),
        );
        if !git_path.is_empty() {
            env.insert(ENV_FAKE_AGENT_GIT_PATH.to_string(), git_path.to_string());
        }
        if !gh_path.is_empty() {
            env.insert(ENV_FAKE_AGENT_GH_PATH.to_string(), gh_path.to_string());
        }
        (vendor, self.path.to_string_lossy().to_string(), env)
    }

    /// Returns the path to the CWD-evidence JSON file.
    pub fn evidence_path(&self) -> PathBuf {
        self.artifact_dir.join("cwd-evidence.json")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fake_agent_new_creates_dir() {
        let bins = BuiltBinaries {
            looper_path: "/bin/looper".into(),
            looperd_path: "/bin/looperd".into(),
            fake_agent_path: "/bin/fake-agent".into(),
            fake_gh_path: "/bin/fake-gh".into(),
            fake_osascript_path: "/bin/fake-osascript".into(),
        };
        let agent = FakeAgent::new(&bins);
        assert!(agent.artifact_dir.exists());
        assert!(agent.state_path.parent().unwrap().exists());
    }

    #[test]
    fn test_fake_agent_config_returns_env() {
        let bins = BuiltBinaries {
            looper_path: "/bin/looper".into(),
            looperd_path: "/bin/looperd".into(),
            fake_agent_path: "/bin/fake-agent".into(),
            fake_gh_path: "/bin/fake-gh".into(),
            fake_osascript_path: "/bin/fake-osascript".into(),
        };
        let agent = FakeAgent::new(&bins);
        let (_vendor, _command, env) = agent.agent_config("cwd-evidence", "/usr/bin/git", "/usr/bin/gh");
        assert!(env.contains_key(ENV_FAKE_AGENT_MODE));
        assert!(env.contains_key(ENV_FAKE_AGENT_ARTIFACT_DIR));
        assert!(env.contains_key(ENV_FAKE_AGENT_GIT_PATH));
        assert!(env.contains_key(ENV_FAKE_AGENT_GH_PATH));
    }

    #[test]
    fn test_evidence_path_format() {
        let bins = BuiltBinaries {
            looper_path: "/bin/looper".into(),
            looperd_path: "/bin/looperd".into(),
            fake_agent_path: "/bin/fake-agent".into(),
            fake_gh_path: "/bin/fake-gh".into(),
            fake_osascript_path: "/bin/fake-osascript".into(),
        };
        let agent = FakeAgent::new(&bins);
        let ep = agent.evidence_path();
        assert!(ep.to_string_lossy().ends_with("cwd-evidence.json"));
    }
}
