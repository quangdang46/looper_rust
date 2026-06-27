use std::path::{Path, PathBuf};

use crate::defaults::discover_config_file;
use crate::env::load_env_overrides;
use crate::error::ConfigError;
use crate::normalize::normalize;
use crate::partial::Merge;
use crate::partial::PartialConfig;
use crate::types::Config;
use crate::validate::validate_config;

use tracing;

/// Builder for the config loading pipeline.
///
/// Default precedence (highest wins):
/// 1. CLI overrides (via `with_cli_overrides`)
/// 2. Environment variables (`LOOPER_*`)
/// 3. Config file
/// 4. Defaults (embedded in the types)
pub struct ConfigLoader {
    config_path: Option<PathBuf>,
    cli_overrides: PartialConfig,
    skip_env: bool,
    skip_file: bool,
}

impl Default for ConfigLoader {
    fn default() -> Self {
        Self::new()
    }
}

impl ConfigLoader {
    /// Create a new loader with sensible defaults.
    pub fn new() -> Self {
        Self { config_path: None, cli_overrides: PartialConfig::default(), skip_env: false, skip_file: false }
    }

    /// Explicitly set the config file path (bypasses auto-discovery).
    pub fn with_config_path(mut self, path: PathBuf) -> Self {
        self.config_path = Some(path);
        self
    }

    /// Set CLI-supplied overrides that take highest precedence.
    pub fn with_cli_overrides(mut self, overrides: PartialConfig) -> Self {
        self.cli_overrides = overrides;
        self
    }

    /// Skip loading from environment variables.
    pub fn skip_env(mut self) -> Self {
        self.skip_env = true;
        self
    }

    /// Skip loading from file (useful for testing or embedded configs).
    pub fn skip_file(mut self) -> Self {
        self.skip_file = true;
        self
    }

    /// Run the full loading pipeline and produce a validated Config.
    pub fn load(self) -> Result<Config, ConfigError> {
        let mut partial = PartialConfig::default();

        // 1. File (lowest precedence within external sources)
        if !self.skip_file {
            let file_partial = self.load_file_partial()?;
            partial.merge(file_partial);
        }

        // 2. Environment variables
        if !self.skip_env {
            let env_partial = load_env_overrides();
            partial.merge(env_partial);
        }

        // 3. CLI overrides (highest precedence)
        partial.merge(self.cli_overrides);

        // 4. Normalize (deprecation warnings + legacy field migration)
        let normalize_issues = normalize(&mut partial);
        if !normalize_issues.is_valid() {
            tracing::warn!("config normalization found warnings");
        }

        // 5. Resolve partial → full config (filling defaults)
        let config: Config = partial.into();

        // 6. Validate
        let validation = validate_config(&config);
        if let Err(e) = validation.into_result() {
            tracing::warn!("config validation issues found");
            return Err(e);
        }

        Ok(config)
    }

    /// Load and parse the config file, handling discovery and format detection.
    fn load_file_partial(&self) -> Result<PartialConfig, ConfigError> {
        let path = match &self.config_path {
            Some(p) => {
                if !p.exists() {
                    return Err(ConfigError::FileNotFound(p.display().to_string()));
                }
                p.clone()
            }
            None => discover_config_file()
                .ok_or_else(|| ConfigError::FileNotFound("no config file found in search paths".into()))?,
        };

        let content = std::fs::read_to_string(&path)
            .map_err(|e| ConfigError::ReadError { path: path.display().to_string(), source: e })?;

        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("toml").to_lowercase();

        parse_config_content(&content, &ext)
            .map_err(|detail| ConfigError::ParseError { path: path.display().to_string(), detail })
    }
}

/// Parse config content in TOML, YAML, or JSON format.
///
/// Detection is by file extension: `.toml` → toml, `.yaml`/`.yml` → yaml,
/// `.json` → json.  Falls back to TOML if the extension is unrecognised.
pub fn parse_config_content(content: &str, ext: &str) -> Result<PartialConfig, String> {
    match ext {
        "yaml" | "yml" => serde_yaml::from_str(content).map_err(|e| format!("YAML parse error: {e}")),
        "json" => serde_json::from_str(content).map_err(|e| format!("JSON parse error: {e}")),
        // default: toml
        _ => toml::from_str(content).map_err(|e| format!("TOML parse error: {e}")),
    }
}

/// Read and parse a config file directly (convenience).
pub fn load_config_file(path: &Path) -> Result<PartialConfig, ConfigError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| ConfigError::ReadError { path: path.display().to_string(), source: e })?;
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("toml").to_lowercase();
    parse_config_content(&content, &ext)
        .map_err(|detail| ConfigError::ParseError { path: path.display().to_string(), detail })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_toml_minimal() {
        let toml = r#"
            [server]
            host = "0.0.0.0"
            port = 8080
        "#;
        let partial = parse_config_content(toml, "toml").unwrap();
        assert!(partial.server.is_some());
        assert_eq!(partial.server.unwrap().host, Some("0.0.0.0".into()));
    }

    #[test]
    fn test_parse_invalid_toml() {
        let result = parse_config_content("<<bad>>", "toml");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_json_minimal() {
        let json = r#"{"server": {"host": "0.0.0.0", "port": 8080}}"#;
        let partial = parse_config_content(json, "json").unwrap();
        assert!(partial.server.is_some());
    }

    #[test]
    fn test_loader_with_cli_overrides() {
        let mut cli = PartialConfig::default();
        let server = crate::partial::PartialServerConfig { host: Some("cli-host".into()), ..Default::default() };
        cli.server = Some(server);

        let config = ConfigLoader::new().skip_file().with_cli_overrides(cli).load().unwrap();

        assert_eq!(config.server.unwrap().host, "cli-host");
    }

    #[test]
    fn test_loader_no_file_returns_error() {
        let result = ConfigLoader::new().skip_env().load();
        // Should error because no config file exists
        assert!(result.is_err());
    }
}
