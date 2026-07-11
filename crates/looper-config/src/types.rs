use crate::enums::{
    DaemonRestartPolicy, DisclosureFormat, LogFormat, LogOutput, LogRotation, NotificationChannel,
    NotificationPriority, SchedulerPolicy, StorageBackend, ToolRuntime,
};
use looper_types::{AgentVendor, DaemonMode, LogLevel};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Top-level Config
// ---------------------------------------------------------------------------

/// The canonical Looper configuration tree.
///
/// All fields are optional at the top-level so the user can provide a sparse
/// file; missing sections fall through to defaults or env-var overrides via
/// the [`PartialConfig`] merge pipeline.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    pub server: Option<ServerConfig>,
    pub daemon: Option<DaemonConfig>,
    pub storage: Option<StorageConfig>,
    pub scheduler: Option<SchedulerConfig>,
    pub agent: Option<AgentConfig>,
    pub logging: Option<LoggingConfig>,
    pub notifications: Option<NotificationsConfig>,
    pub disclosure: Option<DisclosureConfig>,
    pub tools: Option<ToolsConfig>,
    pub package: Option<PackageConfig>,
    pub defaults: Option<DefaultsConfig>,
    pub instructions: Option<InstructionsConfig>,
    pub roles: Option<RolesConfig>,
    pub dispatch: Option<DispatchConfig>,
    #[serde(default)]
    pub projects: Vec<ProjectConfig>,
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub tls_cert: Option<String>,
    pub tls_key: Option<String>,
    pub allowed_origins: Vec<String>,
    pub read_timeout_secs: u64,
    pub write_timeout_secs: u64,
    pub max_body_size_mb: u64,
    pub cors_enabled: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 7391,
            tls_cert: None,
            tls_key: None,
            allowed_origins: vec!["http://localhost:7391".into()],
            read_timeout_secs: 30,
            write_timeout_secs: 60,
            max_body_size_mb: 10,
            cors_enabled: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Daemon
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct DaemonConfig {
    pub mode: DaemonMode,
    pub pid_file: Option<String>,
    pub log_file: Option<String>,
    pub max_restarts: u32,
    pub restart_delay_secs: u64,
    pub restart_policy: DaemonRestartPolicy,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            mode: DaemonMode::Local,
            pid_file: None,
            log_file: None,
            max_restarts: 5,
            restart_delay_secs: 5,
            restart_policy: DaemonRestartPolicy::OnFailure,
        }
    }
}

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct StorageConfig {
    pub backend: StorageBackend,
    pub path: Option<String>,
    pub connection_string: Option<String>,
    pub pool_size: u32,
    pub timeout_secs: u64,
    pub migration_dir: Option<String>,
    pub wal_enabled: bool,
    pub retry_on_busy: bool,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            backend: StorageBackend::Sqlite,
            path: None,
            connection_string: None,
            pool_size: 4,
            timeout_secs: 30,
            migration_dir: None,
            wal_enabled: true,
            retry_on_busy: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Scheduler
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct SchedulerConfig {
    pub policy: SchedulerPolicy,
    pub max_workers: u32,
    pub queue_capacity: u32,
    pub poll_interval_ms: u64,
    pub claim_timeout_secs: u64,
    pub retry: SchedulerRetryConfig,
    pub graceful_shutdown_secs: u64,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            policy: SchedulerPolicy::Fifo,
            max_workers: 4,
            queue_capacity: 256,
            poll_interval_ms: 1000,
            claim_timeout_secs: 300,
            retry: SchedulerRetryConfig::default(),
            graceful_shutdown_secs: 30,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct SchedulerRetryConfig {
    pub max_attempts: u32,
    pub base_delay_secs: u64,
    pub max_delay_secs: u64,
    pub multiplier: f64,
    pub jitter: bool,
}

impl Default for SchedulerRetryConfig {
    fn default() -> Self {
        Self { max_attempts: 3, base_delay_secs: 5, max_delay_secs: 300, multiplier: 2.0, jitter: true }
    }
}

// ---------------------------------------------------------------------------
// Agent
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct AgentConfig {
    pub default_vendor: AgentVendor,
    pub timeout_secs: u64,
    pub max_retries: u32,
    pub parallel_runs: u32,
    pub model: Option<String>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<u32>,
    pub allowed_vendors: Vec<AgentVendor>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            default_vendor: AgentVendor::Claude,
            timeout_secs: 120,
            max_retries: 3,
            parallel_runs: 1,
            model: None,
            temperature: None,
            max_tokens: None,
            allowed_vendors: vec![],
        }
    }
}

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct LoggingConfig {
    pub level: LogLevel,
    pub format: LogFormat,
    pub output: LogOutput,
    pub file_path: Option<String>,
    pub max_files: u32,
    pub max_size_mb: u64,
    pub rotation: LogRotation,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: LogLevel::Info,
            format: LogFormat::Text,
            output: LogOutput::Stdout,
            file_path: None,
            max_files: 7,
            max_size_mb: 100,
            rotation: LogRotation::Daily,
        }
    }
}

// ---------------------------------------------------------------------------
// Notifications
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct NotificationsConfig {
    pub enabled: bool,
    pub min_priority: NotificationPriority,
    pub channels: Vec<NotificationChannel>,
    pub webhook_url: Option<String>,
    pub slack_webhook: Option<String>,
    pub email_smtp: Option<String>,
    pub desktop_notifications: bool,
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_priority: NotificationPriority::Normal,
            channels: vec![NotificationChannel::Log],
            webhook_url: None,
            slack_webhook: None,
            email_smtp: None,
            desktop_notifications: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Disclosure
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct DisclosureConfig {
    pub format: DisclosureFormat,
    pub include_code: bool,
    pub include_diffs: bool,
    pub include_metadata: bool,
    pub protected_phrases: Vec<String>,
    pub max_length: Option<u64>,
    pub stamp: DisclosureStampConfig,
}

impl Default for DisclosureConfig {
    fn default() -> Self {
        Self {
            format: DisclosureFormat::Markdown,
            include_code: true,
            include_diffs: true,
            include_metadata: true,
            protected_phrases: vec![],
            max_length: None,
            stamp: DisclosureStampConfig::default(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct DisclosureStampConfig {
    pub enabled: bool,
    pub prefix: String,
    pub include_timestamp: bool,
    pub include_config_hash: bool,
}

impl Default for DisclosureStampConfig {
    fn default() -> Self {
        Self { enabled: true, prefix: "ai-generated".into(), include_timestamp: true, include_config_hash: true }
    }
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct ToolsConfig {
    pub runtime: ToolRuntime,
    pub allow_network: bool,
    pub timeout_secs: u64,
    pub cache_enabled: bool,
    pub cache_dir: Option<String>,
    pub allowed_paths: Vec<String>,
    pub denied_paths: Vec<String>,
    pub docker: Option<DockerConfig>,
    pub nix: Option<NixConfig>,
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            runtime: ToolRuntime::Host,
            allow_network: true,
            timeout_secs: 300,
            cache_enabled: true,
            cache_dir: None,
            allowed_paths: vec![],
            denied_paths: vec![],
            docker: None,
            nix: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct DockerConfig {
    pub image: String,
    pub tag: String,
    pub volumes: Vec<String>,
    pub network: String,
}

impl Default for DockerConfig {
    fn default() -> Self {
        Self { image: "looper/tools".into(), tag: "latest".into(), volumes: vec![], network: "host".into() }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
#[derive(Default)]
pub struct NixConfig {
    pub packages: Vec<String>,
    pub extra_nix_path: Option<String>,
}

// ---------------------------------------------------------------------------
// Package
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct PackageConfig {
    pub name: String,
    pub version: String,
    pub registry: Option<String>,
    pub install_dir: Option<String>,
}

impl Default for PackageConfig {
    fn default() -> Self {
        Self { name: "looper".into(), version: "0.1.0".into(), registry: None, install_dir: None }
    }
}

// ---------------------------------------------------------------------------
// Defaults
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
#[derive(Default)]
pub struct DefaultsConfig {
    pub home_dir: Option<String>,
    pub config_dir: Option<String>,
    pub data_dir: Option<String>,
    pub cache_dir: Option<String>,
    pub log_dir: Option<String>,
    pub editor: Option<String>,
    pub shell: Option<String>,
}

// ---------------------------------------------------------------------------
// Instructions
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct InstructionsConfig {
    pub base_path: Option<String>,
    pub encoding: String,
    pub max_size_kb: u64,
    pub allowed_extensions: Vec<String>,
}

impl Default for InstructionsConfig {
    fn default() -> Self {
        Self {
            base_path: None,
            encoding: "utf-8".into(),
            max_size_kb: 1024,
            allowed_extensions: vec![".md".into(), ".txt".into()],
        }
    }
}

// ---------------------------------------------------------------------------
// Roles
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
#[derive(Default)]
pub struct RolesConfig {
    pub planner: RoleConfig,
    pub reviewer: RoleConfig,
    pub worker: RoleConfig,
    pub fixer: RoleConfig,
    pub coordinator: RoleConfig,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct RoleConfig {
    pub enabled: bool,
    pub model: Option<String>,
    pub instructions_path: Option<String>,
    pub timeout_secs: u64,
    pub max_retries: u32,
    pub parallel_tasks: u32,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f64>,
}

impl Default for RoleConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            model: None,
            instructions_path: None,
            timeout_secs: 300,
            max_retries: 3,
            parallel_tasks: 1,
            max_tokens: None,
            temperature: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Project
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct ProjectConfig {
    pub name: String,
    pub path: Option<String>,
    pub default_loop_type: Option<String>,
    pub schedule: Option<String>,
    pub enabled: bool,
}

impl Default for ProjectConfig {
    fn default() -> Self {
        Self { name: String::new(), path: None, default_loop_type: None, schedule: None, enabled: true }
    }
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DispatchMode {
    #[serde(rename = "human-gated")]
    HumanGated,
    #[default]
    #[serde(rename = "autonomous")]
    Autonomous,
}

impl std::str::FromStr for DispatchMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "human-gated" => Ok(Self::HumanGated),
            "autonomous" => Ok(Self::Autonomous),
            _ => Err(format!("unknown dispatch mode: {s}")),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct DispatchConfig {
    pub mode: DispatchMode,
    pub allowed_users: Vec<String>,
    pub triaged_label: String,
    pub hold_label: String,
    pub autonomous_delay_seconds: u64,
    pub slash_commands: Vec<String>,
}

impl Default for DispatchConfig {
    fn default() -> Self {
        Self {
            mode: DispatchMode::Autonomous,
            allowed_users: vec![],
            triaged_label: "looper:triaged".into(),
            hold_label: "looper:hold".into(),
            autonomous_delay_seconds: 1800,
            slash_commands: vec!["/plan".into(), "/implement".into()],
        }
    }
}

// ---------------------------------------------------------------------------
// Config file discovery constants
// ---------------------------------------------------------------------------

/// Possible config file names (in preference order).
pub const CONFIG_FILE_NAMES: &[&str] = &["looper.toml", "looper.yaml", "looper.yml", "looper.json"];
pub const CONFIG_DIR_NAME: &str = "looper";
