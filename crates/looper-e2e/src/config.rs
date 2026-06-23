use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

use looper_config::types::Config;

use crate::temp_home::TempHome;

/// Paths to external tools that the E2E config may override.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TestToolPaths {
    /// Path to `git` binary.
    pub git: String,
    /// Path to `gh` CLI.
    pub gh: String,
    /// Path to the `looper` CLI binary.
    pub looper: String,
    /// Path to the `osascript` binary (macOS notifications).
    pub osascript: String,
}

/// Tunable options for [`default_config`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConfigOptions {
    /// TCP port for the daemon (0 = use default).
    pub port: Option<u16>,
    /// Explicit working directory; defaults to `home.working_dir`.
    pub working_dir: Option<String>,
    /// Override tool paths.
    pub tool_paths: TestToolPaths,
    /// Vendor override for the agent config.
    pub agent_vendor: Option<looper_types::AgentVendor>,
    /// Explicit command override for the agent.
    pub agent_command: Option<String>,
    /// Extra environment variables to pass to the agent.
    pub agent_env: HashMap<String, String>,
    /// List of project references to inject.
    pub projects: Vec<serde_json::Value>,
    /// Enable osascript notification backend.
    pub enable_osascript: bool,
    /// Disable disclosure stamps.
    pub disable_disclosure: bool,
}

/// Build a default [`Config`] adjusted for E2E test usage.
///
/// Sets `HOME` to `home.home_dir` in the current env, attaches storage
/// paths, worktree root, and agent options from the given
/// [`ConfigOptions`].
///
/// # Panics
/// Panics if env var modification fails or config defaults fail.
pub fn default_config(home: &TempHome, options: ConfigOptions) -> Config {
    let _working_dir = options
        .working_dir
        .clone()
        .unwrap_or_else(|| home.working_dir.to_string_lossy().to_string());

    std::env::set_var("HOME", home.home_dir.to_string_lossy().as_ref());

    // Overlay storage, daemon, tool paths, agent, etc.
    // Use the partial config merge approach to fill in E2E overrides.
    let mut partial = looper_config::partial::PartialConfig::default();

    // Point server at the configured port (or default port).
    partial.server = Some(looper_config::partial::PartialServerConfig {
        host: Some("127.0.0.1".into()),
        port: options.port,
        ..Default::default()
    });

    partial.storage = Some(looper_config::partial::PartialStorageConfig {
        path: Some(home.db_path.to_string_lossy().to_string()),
        ..Default::default()
    });

    partial.daemon = Some(looper_config::partial::PartialDaemonConfig {
        log_file: Some(home.log_dir.join("looperd.log").to_string_lossy().to_string()),
        ..Default::default()
    });

    partial.defaults = Some(looper_config::partial::PartialDefaultsConfig {
        home_dir: Some(home.home_dir.to_string_lossy().to_string()),
        ..Default::default()
    });

    // Apply agent overrides if provided.
    if let Some(vendor) = &options.agent_vendor {
        let mut agent = looper_config::partial::PartialAgentConfig::default();
        if options.agent_command.is_some() {
            agent.default_vendor = Some(*vendor);
        }
        partial.agent = Some(agent);
    }

    // Apply tool paths (placeholder — may be wired when looper-config grows
    // tool-path fields).
    let tools = looper_config::partial::PartialToolsConfig::default();
    partial.tools = Some(tools);

    // Convert partial into the full config (defaults filled in).
    partial.into()
}

/// Write an E2E config file at `path`, applying `raw_overrides` via deep
/// merge on top of the serialised JSON.
///
/// # Panics
/// Panics on serialisation or I/O error.
pub fn write_config(
    path: impl AsRef<Path>,
    cfg: &Config,
    raw_overrides: HashMap<String, serde_json::Value>,
) {
    // Serialise the config to a JSON map.
    let json_str =
        serde_json::to_string_pretty(cfg).expect("serialise config for E2E test");

    let mut doc: serde_json::Value =
        serde_json::from_str(&json_str).expect("decode config as JSON value");

    // Apply raw overrides via deep merge.
    deep_merge(&mut doc, &serde_json::Value::Object(raw_overrides.into_iter().collect()));

    // Ensure parent directory exists.
    if let Some(parent) = path.as_ref().parent() {
        std::fs::create_dir_all(parent).expect("create config parent directory");
    }

    let formatted = serde_json::to_string_pretty(&doc).expect("format config JSON");
    std::fs::write(path.as_ref(), formatted).expect("write E2E config file");
}

/// Recursively merge `src` into `dst` for JSON objects; for non-objects,
/// `src` overwrites `dst`.
fn deep_merge(dst: &mut serde_json::Value, src: &serde_json::Value) {
    match (dst, src) {
        (serde_json::Value::Object(dst_map), serde_json::Value::Object(src_map)) => {
            for (key, src_val) in src_map {
                if let Some(dst_val) = dst_map.get_mut(key) {
                    deep_merge(dst_val, src_val);
                } else {
                    dst_map.insert(key.clone(), src_val.clone());
                }
            }
        }
        (dst, src) => {
            *dst = src.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deep_merge_object_overrides() {
        let mut dst = serde_json::json!({"a": 1, "b": {"c": 2}});
        let src = serde_json::json!({"b": {"d": 3}});
        deep_merge(&mut dst, &src);
        assert_eq!(dst, serde_json::json!({"a": 1, "b": {"c": 2, "d": 3}}));
    }

    #[test]
    fn test_deep_merge_replaces_non_object() {
        let mut dst = serde_json::json!({"a": {"b": 1}});
        let src = serde_json::json!({"a": 2});
        deep_merge(&mut dst, &src);
        assert_eq!(dst, serde_json::json!({"a": 2}));
    }

    #[test]
    fn test_default_config_no_options() {
        let home = TempHome::new("test");
        let cfg = default_config(&home, ConfigOptions::default());
        // Should not panic; cfg fields should have sensible defaults
        assert!(cfg.defaults.is_some());
        home.cleanup();
    }

    #[test]
    fn test_write_config_overrides() {
        let home = TempHome::new("test");
        let cfg = default_config(&home, ConfigOptions::default());
        let overrides = HashMap::from([(
            "server".to_string(),
            serde_json::json!({"port": 9999}),
        )]);
        write_config(&home.config_path, &cfg, overrides);

        let content = std::fs::read_to_string(&home.config_path).unwrap();
        let loaded: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(loaded["server"]["port"], 9999);
        home.cleanup();
    }
}
