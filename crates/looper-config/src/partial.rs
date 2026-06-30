use crate::enums::{
    DaemonRestartPolicy, DisclosureFormat, LogFormat, LogOutput, LogRotation, NotificationChannel,
    NotificationPriority, SchedulerPolicy, StorageBackend, ToolRuntime,
};
use looper_types::{AgentVendor, DaemonMode, LogLevel};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Merge trait — overlay `other` onto `self`, non-None/Option wins
// ---------------------------------------------------------------------------

pub trait Merge {
    fn merge(&mut self, other: Self);
}

// Primitive types (merge = replace if other is Some)
impl<T> Merge for Option<T> {
    fn merge(&mut self, other: Self) {
        if other.is_some() {
            *self = other;
        }
    }
}

// Vec: merge = extend
impl<T> Merge for Vec<T> {
    fn merge(&mut self, other: Self) {
        self.extend(other);
    }
}

// String: merge = replace if not empty
impl Merge for String {
    fn merge(&mut self, other: Self) {
        if !other.is_empty() {
            *self = other;
        }
    }
}

// ---------------------------------------------------------------------------
// PartialConfig — all fields are Option, set by file/env/CLI sources
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct PartialConfig {
    pub server: Option<PartialServerConfig>,
    pub daemon: Option<PartialDaemonConfig>,
    pub storage: Option<PartialStorageConfig>,
    pub scheduler: Option<PartialSchedulerConfig>,
    pub agent: Option<PartialAgentConfig>,
    pub logging: Option<PartialLoggingConfig>,
    pub notifications: Option<PartialNotificationsConfig>,
    pub disclosure: Option<PartialDisclosureConfig>,
    pub tools: Option<PartialToolsConfig>,
    pub package: Option<PartialPackageConfig>,
    pub defaults: Option<PartialDefaultsConfig>,
    pub instructions: Option<PartialInstructionsConfig>,
    pub roles: Option<PartialRolesConfig>,
    pub dispatch: Option<PartialDispatchConfig>,
    #[serde(default)]
    pub projects: Option<Vec<PartialProjectConfig>>,
}

impl Merge for PartialConfig {
    fn merge(&mut self, other: Self) {
        self.server.merge(other.server);
        self.daemon.merge(other.daemon);
        self.storage.merge(other.storage);
        self.scheduler.merge(other.scheduler);
        self.agent.merge(other.agent);
        self.logging.merge(other.logging);
        self.notifications.merge(other.notifications);
        self.disclosure.merge(other.disclosure);
        self.tools.merge(other.tools);
        self.package.merge(other.package);
        self.defaults.merge(other.defaults);
        self.instructions.merge(other.instructions);
        self.roles.merge(other.roles);
        self.dispatch.merge(other.dispatch);
        self.projects.merge(other.projects);
    }
}

macro_rules! partial_merge_impl {
    ($t:ty { $($field:ident),+ $(,)? }) => {
        impl Merge for $t {
            fn merge(&mut self, other: Self) {
                $(self.$field.merge(other.$field);)+
            }
        }
    };
}

macro_rules! partial_default_impl {
    ($t:ty { $($field:ident $(: $val:expr)?),+ $(,)? }) => {
        impl Default for $t {
            fn default() -> Self {
                Self {
                    $($field: None $(.or(Some($val)))?),+
                }
            }
        }
    };
}

// ---------------------------------------------------------------------------
// Partial Server
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct PartialServerConfig {
    pub host: Option<String>,
    pub port: Option<u16>,
    pub tls_cert: Option<String>,
    pub tls_key: Option<String>,
    pub allowed_origins: Option<Vec<String>>,
    pub read_timeout_secs: Option<u64>,
    pub write_timeout_secs: Option<u64>,
    pub max_body_size_mb: Option<u64>,
    pub cors_enabled: Option<bool>,
    /// Deprecated in v0.2.0 — use `logging.format` instead.
    /// Automatically migrated by [`normalize`].
    #[serde(skip_serializing)]
    pub log_format: Option<crate::enums::LogFormat>,
}

partial_default_impl!(PartialServerConfig {
    host,
    port,
    tls_cert,
    tls_key,
    allowed_origins,
    read_timeout_secs,
    write_timeout_secs,
    max_body_size_mb,
    cors_enabled,
    log_format,
});

partial_merge_impl!(PartialServerConfig {
    host,
    port,
    tls_cert,
    tls_key,
    allowed_origins,
    read_timeout_secs,
    write_timeout_secs,
    max_body_size_mb,
    cors_enabled,
    log_format,
});

// ---------------------------------------------------------------------------
// Partial Daemon
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct PartialDaemonConfig {
    pub mode: Option<DaemonMode>,
    pub pid_file: Option<String>,
    pub log_file: Option<String>,
    pub max_restarts: Option<u32>,
    pub restart_delay_secs: Option<u64>,
    pub restart_policy: Option<DaemonRestartPolicy>,
    /// Deprecated in v0.2.0 — auto-upgrade is now always enabled.
    #[serde(skip_serializing)]
    pub auto_upgrade: Option<bool>,
}

partial_default_impl!(PartialDaemonConfig {
    mode,
    pid_file,
    log_file,
    max_restarts,
    restart_delay_secs,
    restart_policy,
    auto_upgrade,
});

partial_merge_impl!(PartialDaemonConfig {
    mode,
    pid_file,
    log_file,
    max_restarts,
    restart_delay_secs,
    restart_policy,
    auto_upgrade,
});

// ---------------------------------------------------------------------------
// Partial Storage
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct PartialStorageConfig {
    pub backend: Option<StorageBackend>,
    pub path: Option<String>,
    pub connection_string: Option<String>,
    pub pool_size: Option<u32>,
    pub timeout_secs: Option<u64>,
    pub migration_dir: Option<String>,
    pub wal_enabled: Option<bool>,
    pub retry_on_busy: Option<bool>,
}

partial_default_impl!(PartialStorageConfig {
    backend,
    path,
    connection_string,
    pool_size,
    timeout_secs,
    migration_dir,
    wal_enabled,
    retry_on_busy,
});

partial_merge_impl!(PartialStorageConfig {
    backend,
    path,
    connection_string,
    pool_size,
    timeout_secs,
    migration_dir,
    wal_enabled,
    retry_on_busy,
});

// ---------------------------------------------------------------------------
// Partial Scheduler
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct PartialSchedulerConfig {
    pub policy: Option<SchedulerPolicy>,
    pub max_workers: Option<u32>,
    pub queue_capacity: Option<u32>,
    pub poll_interval_ms: Option<u64>,
    pub claim_timeout_secs: Option<u64>,
    pub retry: Option<PartialSchedulerRetryConfig>,
    pub graceful_shutdown_secs: Option<u64>,
}

partial_default_impl!(PartialSchedulerConfig {
    policy,
    max_workers,
    queue_capacity,
    poll_interval_ms,
    claim_timeout_secs,
    retry,
    graceful_shutdown_secs,
});

partial_merge_impl!(PartialSchedulerConfig {
    policy,
    max_workers,
    queue_capacity,
    poll_interval_ms,
    claim_timeout_secs,
    retry,
    graceful_shutdown_secs,
});

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct PartialSchedulerRetryConfig {
    pub max_attempts: Option<u32>,
    pub base_delay_secs: Option<u64>,
    pub max_delay_secs: Option<u64>,
    pub multiplier: Option<f64>,
    pub jitter: Option<bool>,
}

partial_default_impl!(PartialSchedulerRetryConfig {
    max_attempts,
    base_delay_secs,
    max_delay_secs,
    multiplier,
    jitter,
});

partial_merge_impl!(PartialSchedulerRetryConfig { max_attempts, base_delay_secs, max_delay_secs, multiplier, jitter });

// ---------------------------------------------------------------------------
// Partial Agent
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct PartialAgentConfig {
    pub default_vendor: Option<AgentVendor>,
    pub timeout_secs: Option<u64>,
    pub max_retries: Option<u32>,
    pub parallel_runs: Option<u32>,
    pub model: Option<String>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<u32>,
    pub allowed_vendors: Option<Vec<AgentVendor>>,
    /// Deprecated in v0.3.0 — use `model` instead.
    #[serde(skip_serializing)]
    pub command: Option<String>,
}

partial_default_impl!(PartialAgentConfig {
    default_vendor,
    timeout_secs,
    max_retries,
    parallel_runs,
    model,
    temperature,
    max_tokens,
    allowed_vendors,
    command,
});

partial_merge_impl!(PartialAgentConfig {
    default_vendor,
    timeout_secs,
    max_retries,
    parallel_runs,
    model,
    temperature,
    max_tokens,
    allowed_vendors,
    command,
});

// ---------------------------------------------------------------------------
// Partial Logging
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct PartialLoggingConfig {
    pub level: Option<LogLevel>,
    pub format: Option<LogFormat>,
    pub output: Option<LogOutput>,
    pub file_path: Option<String>,
    pub max_files: Option<u32>,
    pub max_size_mb: Option<u64>,
    pub rotation: Option<LogRotation>,
}

partial_default_impl!(PartialLoggingConfig { level, format, output, file_path, max_files, max_size_mb, rotation });

partial_merge_impl!(PartialLoggingConfig { level, format, output, file_path, max_files, max_size_mb, rotation });

// ---------------------------------------------------------------------------
// Partial Notifications
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct PartialNotificationsConfig {
    pub enabled: Option<bool>,
    pub min_priority: Option<NotificationPriority>,
    pub channels: Option<Vec<NotificationChannel>>,
    pub webhook_url: Option<String>,
    pub slack_webhook: Option<String>,
    pub email_smtp: Option<String>,
    pub desktop_notifications: Option<bool>,
    /// Legacy alias; deserialized from `webhook-url` via serde.
    #[serde(skip_serializing)]
    pub legacy_webhook_url: Option<String>,
}

partial_default_impl!(PartialNotificationsConfig {
    enabled,
    min_priority,
    channels,
    webhook_url,
    slack_webhook,
    email_smtp,
    desktop_notifications,
    legacy_webhook_url,
});

partial_merge_impl!(PartialNotificationsConfig {
    enabled,
    min_priority,
    channels,
    webhook_url,
    slack_webhook,
    email_smtp,
    desktop_notifications,
    legacy_webhook_url,
});

// ---------------------------------------------------------------------------
// Partial Disclosure
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct PartialDisclosureConfig {
    pub format: Option<DisclosureFormat>,
    pub include_code: Option<bool>,
    pub include_diffs: Option<bool>,
    pub include_metadata: Option<bool>,
    pub protected_phrases: Option<Vec<String>>,
    pub max_length: Option<u64>,
    pub stamp: Option<PartialDisclosureStampConfig>,
}

partial_default_impl!(PartialDisclosureConfig {
    format,
    include_code,
    include_diffs,
    include_metadata,
    protected_phrases,
    max_length,
    stamp,
});

partial_merge_impl!(PartialDisclosureConfig {
    format,
    include_code,
    include_diffs,
    include_metadata,
    protected_phrases,
    max_length,
    stamp,
});

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct PartialDisclosureStampConfig {
    pub enabled: Option<bool>,
    pub prefix: Option<String>,
    pub include_timestamp: Option<bool>,
    pub include_config_hash: Option<bool>,
}

partial_default_impl!(PartialDisclosureStampConfig { enabled, prefix, include_timestamp, include_config_hash });

partial_merge_impl!(PartialDisclosureStampConfig { enabled, prefix, include_timestamp, include_config_hash });

// ---------------------------------------------------------------------------
// Partial Tools
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct PartialToolsConfig {
    pub runtime: Option<ToolRuntime>,
    pub allow_network: Option<bool>,
    pub timeout_secs: Option<u64>,
    pub cache_enabled: Option<bool>,
    pub cache_dir: Option<String>,
    pub allowed_paths: Option<Vec<String>>,
    pub denied_paths: Option<Vec<String>>,
    pub docker: Option<PartialDockerConfig>,
    pub nix: Option<PartialNixConfig>,
    /// Deprecated in v0.3.0 — use `runtime` instead.
    #[serde(skip_serializing)]
    pub allow_docker: Option<bool>,
}

partial_default_impl!(PartialToolsConfig {
    runtime,
    allow_network,
    timeout_secs,
    cache_enabled,
    cache_dir,
    allowed_paths,
    denied_paths,
    docker,
    nix,
    allow_docker,
});

partial_merge_impl!(PartialToolsConfig {
    runtime,
    allow_network,
    timeout_secs,
    cache_enabled,
    cache_dir,
    allowed_paths,
    denied_paths,
    docker,
    nix,
    allow_docker,
});

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct PartialDockerConfig {
    pub image: Option<String>,
    pub tag: Option<String>,
    pub volumes: Option<Vec<String>>,
    pub network: Option<String>,
}

partial_default_impl!(PartialDockerConfig { image, tag, volumes, network });

partial_merge_impl!(PartialDockerConfig { image, tag, volumes, network });

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct PartialNixConfig {
    pub packages: Option<Vec<String>>,
    pub extra_nix_path: Option<String>,
}

partial_default_impl!(PartialNixConfig { packages, extra_nix_path });

partial_merge_impl!(PartialNixConfig { packages, extra_nix_path });

// ---------------------------------------------------------------------------
// Partial Package
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct PartialPackageConfig {
    pub name: Option<String>,
    pub version: Option<String>,
    pub registry: Option<String>,
    pub install_dir: Option<String>,
}

partial_default_impl!(PartialPackageConfig { name, version, registry, install_dir });

partial_merge_impl!(PartialPackageConfig { name, version, registry, install_dir });

// ---------------------------------------------------------------------------
// Partial Defaults
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct PartialDefaultsConfig {
    pub home_dir: Option<String>,
    pub config_dir: Option<String>,
    pub data_dir: Option<String>,
    pub cache_dir: Option<String>,
    pub log_dir: Option<String>,
    pub editor: Option<String>,
    pub shell: Option<String>,
}

partial_default_impl!(PartialDefaultsConfig { home_dir, config_dir, data_dir, cache_dir, log_dir, editor, shell });

partial_merge_impl!(PartialDefaultsConfig { home_dir, config_dir, data_dir, cache_dir, log_dir, editor, shell });

// ---------------------------------------------------------------------------
// Partial Instructions
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct PartialInstructionsConfig {
    pub base_path: Option<String>,
    pub encoding: Option<String>,
    pub max_size_kb: Option<u64>,
    pub allowed_extensions: Option<Vec<String>>,
}

partial_default_impl!(PartialInstructionsConfig { base_path, encoding, max_size_kb, allowed_extensions });

partial_merge_impl!(PartialInstructionsConfig { base_path, encoding, max_size_kb, allowed_extensions });

// ---------------------------------------------------------------------------
// Partial Roles
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct PartialRolesConfig {
    pub planner: Option<PartialRoleConfig>,
    pub reviewer: Option<PartialRoleConfig>,
    pub worker: Option<PartialRoleConfig>,
    pub fixer: Option<PartialRoleConfig>,
    pub coordinator: Option<PartialRoleConfig>,
}

partial_default_impl!(PartialRolesConfig { planner, reviewer, worker, fixer, coordinator });

partial_merge_impl!(PartialRolesConfig { planner, reviewer, worker, fixer, coordinator });

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct PartialRoleConfig {
    pub enabled: Option<bool>,
    pub model: Option<String>,
    pub instructions_path: Option<String>,
    pub timeout_secs: Option<u64>,
    pub max_retries: Option<u32>,
    pub parallel_tasks: Option<u32>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f64>,
}

partial_default_impl!(PartialRoleConfig {
    enabled,
    model,
    instructions_path,
    timeout_secs,
    max_retries,
    parallel_tasks,
    max_tokens,
    temperature,
});

partial_merge_impl!(PartialRoleConfig {
    enabled,
    model,
    instructions_path,
    timeout_secs,
    max_retries,
    parallel_tasks,
    max_tokens,
    temperature,
});

// ---------------------------------------------------------------------------
// Partial Project
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct PartialProjectConfig {
    pub name: Option<String>,
    pub path: Option<String>,
    pub default_loop_type: Option<String>,
    pub schedule: Option<String>,
    pub enabled: Option<bool>,
}

partial_default_impl!(PartialProjectConfig { name, path, default_loop_type, schedule, enabled });

partial_merge_impl!(PartialProjectConfig { name, path, default_loop_type, schedule, enabled });

// ---------------------------------------------------------------------------
// Partial Dispatch
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct PartialDispatchConfig {
    pub mode: Option<crate::types::DispatchMode>,
    pub allowed_users: Option<Vec<String>>,
    pub triaged_label: Option<String>,
    pub hold_label: Option<String>,
    pub autonomous_delay_seconds: Option<u64>,
    pub slash_commands: Option<Vec<String>>,
}

partial_default_impl!(PartialDispatchConfig {
    mode,
    allowed_users,
    triaged_label,
    hold_label,
    autonomous_delay_seconds,
    slash_commands,
});

partial_merge_impl!(PartialDispatchConfig {
    mode,
    allowed_users,
    triaged_label,
    hold_label,
    autonomous_delay_seconds,
    slash_commands,
});

// ---------------------------------------------------------------------------
// Conversion helpers: PartialConfig → Config (resolved with defaults)
// ---------------------------------------------------------------------------

use crate::types::{
    AgentConfig, Config, DaemonConfig, DefaultsConfig, DisclosureConfig, DisclosureStampConfig, DockerConfig,
    InstructionsConfig, LoggingConfig, NixConfig, NotificationsConfig, PackageConfig, ProjectConfig, RoleConfig,
    RolesConfig, SchedulerConfig, SchedulerRetryConfig, ServerConfig, StorageConfig, ToolsConfig,
};

impl From<PartialConfig> for Config {
    fn from(partial: PartialConfig) -> Self {
        Self {
            server: partial.server.map(Into::into),
            daemon: partial.daemon.map(Into::into),
            storage: partial.storage.map(Into::into),
            scheduler: partial.scheduler.map(Into::into),
            agent: partial.agent.map(Into::into),
            logging: partial.logging.map(Into::into),
            notifications: partial.notifications.map(Into::into),
            disclosure: partial.disclosure.map(Into::into),
            tools: partial.tools.map(Into::into),
            package: partial.package.map(Into::into),
            defaults: partial.defaults.map(Into::into),
            instructions: partial.instructions.map(Into::into),
            roles: partial.roles.map(Into::into),
            dispatch: partial.dispatch.map(Into::into),
            projects: partial.projects.map(|v| v.into_iter().map(Into::into).collect()).unwrap_or_default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Manual From impls — each field uses unwrap_or/or against the full type's
// Default so that missing partial values correctly fall back to the embedded
// defaults rather than Rust's Default::default() (which would give 0 / "").
// ---------------------------------------------------------------------------

impl From<PartialServerConfig> for ServerConfig {
    fn from(p: PartialServerConfig) -> Self {
        let d = ServerConfig::default();
        Self {
            host: p.host.unwrap_or(d.host),
            port: p.port.unwrap_or(d.port),
            tls_cert: p.tls_cert.or(d.tls_cert),
            tls_key: p.tls_key.or(d.tls_key),
            allowed_origins: p.allowed_origins.unwrap_or(d.allowed_origins),
            read_timeout_secs: p.read_timeout_secs.unwrap_or(d.read_timeout_secs),
            write_timeout_secs: p.write_timeout_secs.unwrap_or(d.write_timeout_secs),
            max_body_size_mb: p.max_body_size_mb.unwrap_or(d.max_body_size_mb),
            cors_enabled: p.cors_enabled.unwrap_or(d.cors_enabled),
        }
    }
}

impl From<PartialDaemonConfig> for DaemonConfig {
    fn from(p: PartialDaemonConfig) -> Self {
        let d = DaemonConfig::default();
        Self {
            mode: p.mode.unwrap_or(d.mode),
            pid_file: p.pid_file.or(d.pid_file),
            log_file: p.log_file.or(d.log_file),
            max_restarts: p.max_restarts.unwrap_or(d.max_restarts),
            restart_delay_secs: p.restart_delay_secs.unwrap_or(d.restart_delay_secs),
            restart_policy: p.restart_policy.unwrap_or(d.restart_policy),
        }
    }
}

impl From<PartialStorageConfig> for StorageConfig {
    fn from(p: PartialStorageConfig) -> Self {
        let d = StorageConfig::default();
        Self {
            backend: p.backend.unwrap_or(d.backend),
            path: p.path.or(d.path),
            connection_string: p.connection_string.or(d.connection_string),
            pool_size: p.pool_size.unwrap_or(d.pool_size),
            timeout_secs: p.timeout_secs.unwrap_or(d.timeout_secs),
            migration_dir: p.migration_dir.or(d.migration_dir),
            wal_enabled: p.wal_enabled.unwrap_or(d.wal_enabled),
            retry_on_busy: p.retry_on_busy.unwrap_or(d.retry_on_busy),
        }
    }
}

impl From<PartialSchedulerConfig> for SchedulerConfig {
    fn from(p: PartialSchedulerConfig) -> Self {
        let d = SchedulerConfig::default();
        Self {
            policy: p.policy.unwrap_or(d.policy),
            max_workers: p.max_workers.unwrap_or(d.max_workers),
            queue_capacity: p.queue_capacity.unwrap_or(d.queue_capacity),
            poll_interval_ms: p.poll_interval_ms.unwrap_or(d.poll_interval_ms),
            claim_timeout_secs: p.claim_timeout_secs.unwrap_or(d.claim_timeout_secs),
            retry: p.retry.map(Into::into).unwrap_or(d.retry),
            graceful_shutdown_secs: p.graceful_shutdown_secs.unwrap_or(d.graceful_shutdown_secs),
        }
    }
}

impl From<PartialSchedulerRetryConfig> for SchedulerRetryConfig {
    fn from(p: PartialSchedulerRetryConfig) -> Self {
        let d = SchedulerRetryConfig::default();
        Self {
            max_attempts: p.max_attempts.unwrap_or(d.max_attempts),
            base_delay_secs: p.base_delay_secs.unwrap_or(d.base_delay_secs),
            max_delay_secs: p.max_delay_secs.unwrap_or(d.max_delay_secs),
            multiplier: p.multiplier.unwrap_or(d.multiplier),
            jitter: p.jitter.unwrap_or(d.jitter),
        }
    }
}

impl From<PartialAgentConfig> for AgentConfig {
    fn from(p: PartialAgentConfig) -> Self {
        let d = AgentConfig::default();
        Self {
            default_vendor: p.default_vendor.unwrap_or(d.default_vendor),
            timeout_secs: p.timeout_secs.unwrap_or(d.timeout_secs),
            max_retries: p.max_retries.unwrap_or(d.max_retries),
            parallel_runs: p.parallel_runs.unwrap_or(d.parallel_runs),
            model: p.model.or(d.model),
            temperature: p.temperature.or(d.temperature),
            max_tokens: p.max_tokens.or(d.max_tokens),
            allowed_vendors: p.allowed_vendors.unwrap_or(d.allowed_vendors),
        }
    }
}

impl From<PartialLoggingConfig> for LoggingConfig {
    fn from(p: PartialLoggingConfig) -> Self {
        let d = LoggingConfig::default();
        Self {
            level: p.level.unwrap_or(d.level),
            format: p.format.unwrap_or(d.format),
            output: p.output.unwrap_or(d.output),
            file_path: p.file_path.or(d.file_path),
            max_files: p.max_files.unwrap_or(d.max_files),
            max_size_mb: p.max_size_mb.unwrap_or(d.max_size_mb),
            rotation: p.rotation.unwrap_or(d.rotation),
        }
    }
}

impl From<PartialNotificationsConfig> for NotificationsConfig {
    fn from(p: PartialNotificationsConfig) -> Self {
        let d = NotificationsConfig::default();
        Self {
            enabled: p.enabled.unwrap_or(d.enabled),
            min_priority: p.min_priority.unwrap_or(d.min_priority),
            channels: p.channels.unwrap_or(d.channels),
            webhook_url: p.webhook_url.or(d.webhook_url),
            slack_webhook: p.slack_webhook.or(d.slack_webhook),
            email_smtp: p.email_smtp.or(d.email_smtp),
            desktop_notifications: p.desktop_notifications.unwrap_or(d.desktop_notifications),
        }
    }
}

impl From<PartialDisclosureConfig> for DisclosureConfig {
    fn from(p: PartialDisclosureConfig) -> Self {
        let d = DisclosureConfig::default();
        Self {
            format: p.format.unwrap_or(d.format),
            include_code: p.include_code.unwrap_or(d.include_code),
            include_diffs: p.include_diffs.unwrap_or(d.include_diffs),
            include_metadata: p.include_metadata.unwrap_or(d.include_metadata),
            protected_phrases: p.protected_phrases.unwrap_or(d.protected_phrases),
            max_length: p.max_length.or(d.max_length),
            stamp: p.stamp.map(Into::into).unwrap_or(d.stamp),
        }
    }
}

impl From<PartialDisclosureStampConfig> for DisclosureStampConfig {
    fn from(p: PartialDisclosureStampConfig) -> Self {
        let d = DisclosureStampConfig::default();
        Self {
            enabled: p.enabled.unwrap_or(d.enabled),
            prefix: p.prefix.unwrap_or(d.prefix),
            include_timestamp: p.include_timestamp.unwrap_or(d.include_timestamp),
            include_config_hash: p.include_config_hash.unwrap_or(d.include_config_hash),
        }
    }
}

impl From<PartialToolsConfig> for ToolsConfig {
    fn from(p: PartialToolsConfig) -> Self {
        let d = ToolsConfig::default();
        Self {
            runtime: p.runtime.unwrap_or(d.runtime),
            allow_network: p.allow_network.unwrap_or(d.allow_network),
            timeout_secs: p.timeout_secs.unwrap_or(d.timeout_secs),
            cache_enabled: p.cache_enabled.unwrap_or(d.cache_enabled),
            cache_dir: p.cache_dir.or(d.cache_dir),
            allowed_paths: p.allowed_paths.unwrap_or(d.allowed_paths),
            denied_paths: p.denied_paths.unwrap_or(d.denied_paths),
            docker: p.docker.map(Into::into).or(d.docker),
            nix: p.nix.map(Into::into).or(d.nix),
        }
    }
}

impl From<PartialDockerConfig> for DockerConfig {
    fn from(p: PartialDockerConfig) -> Self {
        let d = DockerConfig::default();
        Self {
            image: p.image.unwrap_or(d.image),
            tag: p.tag.unwrap_or(d.tag),
            volumes: p.volumes.unwrap_or(d.volumes),
            network: p.network.unwrap_or(d.network),
        }
    }
}

impl From<PartialNixConfig> for NixConfig {
    fn from(p: PartialNixConfig) -> Self {
        let d = NixConfig::default();
        Self { packages: p.packages.unwrap_or(d.packages), extra_nix_path: p.extra_nix_path.or(d.extra_nix_path) }
    }
}

impl From<PartialPackageConfig> for PackageConfig {
    fn from(p: PartialPackageConfig) -> Self {
        let d = PackageConfig::default();
        Self {
            name: p.name.unwrap_or(d.name),
            version: p.version.unwrap_or(d.version),
            registry: p.registry.or(d.registry),
            install_dir: p.install_dir.or(d.install_dir),
        }
    }
}

impl From<PartialDefaultsConfig> for DefaultsConfig {
    fn from(p: PartialDefaultsConfig) -> Self {
        let d = DefaultsConfig::default();
        Self {
            home_dir: p.home_dir.or(d.home_dir),
            config_dir: p.config_dir.or(d.config_dir),
            data_dir: p.data_dir.or(d.data_dir),
            cache_dir: p.cache_dir.or(d.cache_dir),
            log_dir: p.log_dir.or(d.log_dir),
            editor: p.editor.or(d.editor),
            shell: p.shell.or(d.shell),
        }
    }
}

impl From<PartialInstructionsConfig> for InstructionsConfig {
    fn from(p: PartialInstructionsConfig) -> Self {
        let d = InstructionsConfig::default();
        Self {
            base_path: p.base_path.or(d.base_path),
            encoding: p.encoding.unwrap_or(d.encoding),
            max_size_kb: p.max_size_kb.unwrap_or(d.max_size_kb),
            allowed_extensions: p.allowed_extensions.unwrap_or(d.allowed_extensions),
        }
    }
}

impl From<PartialRoleConfig> for RoleConfig {
    fn from(p: PartialRoleConfig) -> Self {
        let d = RoleConfig::default();
        Self {
            enabled: p.enabled.unwrap_or(d.enabled),
            model: p.model.or(d.model),
            instructions_path: p.instructions_path.or(d.instructions_path),
            timeout_secs: p.timeout_secs.unwrap_or(d.timeout_secs),
            max_retries: p.max_retries.unwrap_or(d.max_retries),
            parallel_tasks: p.parallel_tasks.unwrap_or(d.parallel_tasks),
            max_tokens: p.max_tokens.or(d.max_tokens),
            temperature: p.temperature.or(d.temperature),
        }
    }
}

impl From<PartialProjectConfig> for ProjectConfig {
    fn from(p: PartialProjectConfig) -> Self {
        let d = ProjectConfig::default();
        Self {
            name: p.name.unwrap_or(d.name),
            path: p.path.or(d.path),
            default_loop_type: p.default_loop_type.or(d.default_loop_type),
            schedule: p.schedule.or(d.schedule),
            enabled: p.enabled.unwrap_or(d.enabled),
        }
    }
}

impl From<PartialDispatchConfig> for crate::types::DispatchConfig {
    fn from(p: PartialDispatchConfig) -> Self {
        let d = crate::types::DispatchConfig::default();
        Self {
            mode: p.mode.unwrap_or(d.mode),
            allowed_users: p.allowed_users.unwrap_or(d.allowed_users),
            triaged_label: p.triaged_label.unwrap_or(d.triaged_label),
            hold_label: p.hold_label.unwrap_or(d.hold_label),
            autonomous_delay_seconds: p.autonomous_delay_seconds.unwrap_or(d.autonomous_delay_seconds),
            slash_commands: p.slash_commands.unwrap_or(d.slash_commands),
        }
    }
}

impl From<PartialRolesConfig> for RolesConfig {
    fn from(p: PartialRolesConfig) -> Self {
        Self {
            planner: p.planner.map(Into::into).unwrap_or_default(),
            reviewer: p.reviewer.map(Into::into).unwrap_or_default(),
            worker: p.worker.map(Into::into).unwrap_or_default(),
            fixer: p.fixer.map(Into::into).unwrap_or_default(),
            coordinator: p.coordinator.map(Into::into).unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_merge_option_overrides() {
        let mut a = PartialDaemonConfig::default();
        let b = PartialDaemonConfig { max_restarts: Some(10), ..Default::default() };
        a.merge(b);
        assert_eq!(a.max_restarts, Some(10));
    }

    #[test]
    fn test_merge_option_keeps_existing() {
        let mut a = PartialDaemonConfig { max_restarts: Some(5), ..Default::default() };
        let b = PartialDaemonConfig::default();
        a.merge(b);
        assert_eq!(a.max_restarts, Some(5));
    }

    #[test]
    fn test_partial_to_full_server() {
        let partial = PartialServerConfig { port: Some(8080), ..Default::default() };
        let full: ServerConfig = partial.into();
        assert_eq!(full.port, 8080);
        assert_eq!(full.host, "127.0.0.1"); // from Default
    }

    #[test]
    fn test_partial_config_to_config() {
        let partial = PartialConfig {
            server: Some(PartialServerConfig { host: Some("0.0.0.0".into()), ..Default::default() }),
            ..Default::default()
        };
        let config: Config = partial.into();
        let server = config.server.expect("server should be Some");
        assert_eq!(server.host, "0.0.0.0");
        assert_eq!(server.port, 7391); // from Default
    }

    #[test]
    fn test_merge_vec_extend() {
        let mut a = PartialLoggingConfig::default();
        let b = PartialLoggingConfig { file_path: Some("/tmp/looper.log".into()), ..Default::default() };
        a.merge(b);
        assert_eq!(a.file_path, Some("/tmp/looper.log".into()));
    }

    #[test]
    fn test_empty_partial_merge_is_noop() {
        let mut a = PartialAgentConfig { timeout_secs: Some(60), ..Default::default() };
        let b = PartialAgentConfig::default();
        a.merge(b);
        assert_eq!(a.timeout_secs, Some(60));
    }
}
