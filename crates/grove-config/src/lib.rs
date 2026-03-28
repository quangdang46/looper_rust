mod defaults;
mod init_template;
mod loader;
mod model;
mod paths;
mod validate;

pub use defaults::{
    DEFAULT_ARTIFACTS_DIR_NAME, DEFAULT_CHECKPOINTS_DIR_NAME, DEFAULT_DB_PATH,
    DEFAULT_GROVE_DIR_NAME, DEFAULT_LOGS_DIR_NAME, DEFAULT_PROMPTS_DIR_NAME,
    DEFAULT_STARTUP_PROMPT_PATH, DEFAULT_TMP_DIR_NAME, DEFAULT_TRANSCRIPT_DIR,
};
pub use init_template::DEFAULT_INIT_GROVE_TOML;
pub use loader::{
    LoadedConfig, RequiredTooling, ToolCapability, apply_env_overrides, build_provider_environment,
    detect_required_tooling, load_from_path, load_from_path_with_env, load_from_workspace,
};
pub use model::{
    CheckpointConfig, CircuitBreakerConfig, ExitPolicyConfig, GroveConfig, LoggingConfig,
    MemoryConfig, ReactionConfig, ReservationConfig, RuntimeConfig, SafetyConfig, SchedulerConfig,
};
pub use paths::GrovePaths;
pub use validate::validate_config;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("config file not found: {path}")]
    FileNotFound { path: String },

    #[error("TOML parse error: {source}")]
    TomlParse { source: toml::de::Error },

    #[error("validation error: {field} — {message}")]
    Validation { field: String, message: String },

    #[error("workspace root does not exist: {path}")]
    WorkspaceNotFound { path: String },

    #[error("conflicting config: {a} and {b} are incompatible")]
    Conflict { a: String, b: String },
}

pub const CRATE_PURPOSE: &str = "Configuration loading, validation, and path ownership.";

#[cfg(test)]
mod tests;
