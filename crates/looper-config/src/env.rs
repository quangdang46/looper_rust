use crate::enums::{LogFormat, StorageBackend};
use crate::partial::Merge;
use crate::partial::{
    PartialConfig, PartialDaemonConfig, PartialDispatchConfig, PartialLoggingConfig, PartialNotificationsConfig,
    PartialServerConfig, PartialStorageConfig,
};
use crate::types::DispatchMode;
use looper_types::{DaemonMode, LogLevel};
use std::str::FromStr;

/// Environment variable prefix used by all Looper config vars.
const PREFIX: &str = "LOOPER_";

/// Read all `LOOPER_*` environment variables and produce a `PartialConfig`
/// with the overlaid values.
///
/// The variable naming convention is:
/// `LOOPER_<SECTION>_<FIELD>` in screaming snake case.
///
/// Examples:
/// - `LOOPER_SERVER_HOST` → server.host
/// - `LOOPER_STORAGE_BACKEND` → storage.backend
/// - `LOOPER_AGENT_DEFAULT_VENDOR` → agent.default_vendor
pub fn load_env_overrides() -> PartialConfig {
    let mut partial = PartialConfig::default();

    // -- Server section --
    if let Some(v) = get_env("SERVER_HOST") {
        partial.server(PartialServerConfig { host: Some(v), ..Default::default() });
    }
    if let Some(v) = get_env("SERVER_PORT") {
        let port: u16 = v.parse().ok().unwrap_or(7391);
        partial.server(PartialServerConfig { port: Some(port), ..Default::default() });
    }

    // -- Daemon section --
    if let Some(v) = get_env("DAEMON_MODE") {
        partial.daemon(PartialDaemonConfig { mode: DaemonMode::from_str(&v).ok(), ..Default::default() });
    }
    if let Some(v) = get_env("DAEMON_PID_FILE") {
        partial.daemon(PartialDaemonConfig { pid_file: Some(v), ..Default::default() });
    }

    // -- Storage section --
    if let Some(v) = get_env("STORAGE_BACKEND") {
        partial.storage(PartialStorageConfig { backend: StorageBackend::from_str(&v).ok(), ..Default::default() });
    }
    if let Some(v) = get_env("STORAGE_PATH") {
        partial.storage(PartialStorageConfig { path: Some(v), ..Default::default() });
    }
    if let Some(v) = get_env("STORAGE_CONNECTION_STRING") {
        partial.storage(PartialStorageConfig { connection_string: Some(v), ..Default::default() });
    }

    // -- Logging section --
    if let Some(v) = get_env("LOG_LEVEL") {
        partial.logging(PartialLoggingConfig { level: LogLevel::from_str(&v).ok(), ..Default::default() });
    }
    if let Some(v) = get_env("LOG_FORMAT") {
        partial.logging(PartialLoggingConfig { format: LogFormat::from_str(&v).ok(), ..Default::default() });
    }
    if let Some(v) = get_env("LOG_FILE") {
        partial.logging(PartialLoggingConfig { file_path: Some(v), ..Default::default() });
    }

    // -- Notifications section --
    if let Some(v) = get_env("NOTIFICATIONS_ENABLED") {
        let enabled = v == "true" || v == "1";
        partial.notifications(PartialNotificationsConfig { enabled: Some(enabled), ..Default::default() });
    }
    if let Some(v) = get_env("NOTIFICATIONS_WEBHOOK_URL") {
        partial.notifications(PartialNotificationsConfig { webhook_url: Some(v), ..Default::default() });
    }

    // -- Dispatch section --
    if let Some(v) = get_env("DISPATCH_MODE") {
        partial.dispatch(PartialDispatchConfig { mode: DispatchMode::from_str(&v).ok(), ..Default::default() });
    }
    if let Some(v) = get_env("DISPATCH_ALLOWED_USERS") {
        partial.dispatch(PartialDispatchConfig {
            allowed_users: Some(v.split(',').map(|s| s.trim().to_string()).collect()),
            ..Default::default()
        });
    }
    if let Some(v) = get_env("DISPATCH_AUTONOMOUS_DELAY_SECONDS") {
        let secs: u64 = v.parse().unwrap_or(1800);
        partial.dispatch(PartialDispatchConfig { autonomous_delay_seconds: Some(secs), ..Default::default() });
    }

    partial
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Get the value of `LOOPER_<suffix>` as an `Option<String>`.
fn get_env(suffix: &str) -> Option<String> {
    let full = format!("{}{}", PREFIX, suffix);
    std::env::var(&full).ok()
}

// ---------------------------------------------------------------------------
// Convenience: extension trait to set top-level fields on PartialConfig
// ---------------------------------------------------------------------------

trait PartialConfigExt {
    fn server(&mut self, cfg: PartialServerConfig);
    fn daemon(&mut self, cfg: PartialDaemonConfig);
    fn storage(&mut self, cfg: PartialStorageConfig);
    fn logging(&mut self, cfg: PartialLoggingConfig);
    fn notifications(&mut self, cfg: PartialNotificationsConfig);
    fn dispatch(&mut self, cfg: PartialDispatchConfig);
}

impl PartialConfigExt for PartialConfig {
    fn server(&mut self, cfg: PartialServerConfig) {
        match &mut self.server {
            Some(existing) => {
                existing.merge(cfg);
            }
            None => {
                self.server = Some(cfg);
            }
        }
    }

    fn daemon(&mut self, cfg: PartialDaemonConfig) {
        match &mut self.daemon {
            Some(existing) => {
                existing.merge(cfg);
            }
            None => {
                self.daemon = Some(cfg);
            }
        }
    }

    fn storage(&mut self, cfg: PartialStorageConfig) {
        match &mut self.storage {
            Some(existing) => {
                existing.merge(cfg);
            }
            None => {
                self.storage = Some(cfg);
            }
        }
    }

    fn logging(&mut self, cfg: PartialLoggingConfig) {
        match &mut self.logging {
            Some(existing) => {
                existing.merge(cfg);
            }
            None => {
                self.logging = Some(cfg);
            }
        }
    }

    fn notifications(&mut self, cfg: PartialNotificationsConfig) {
        match &mut self.notifications {
            Some(existing) => {
                existing.merge(cfg);
            }
            None => {
                self.notifications = Some(cfg);
            }
        }
    }

    fn dispatch(&mut self, cfg: PartialDispatchConfig) {
        match &mut self.dispatch {
            Some(existing) => {
                existing.merge(cfg);
            }
            None => {
                self.dispatch = Some(cfg);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_server_host_env() {
        std::env::set_var("LOOPER_SERVER_HOST", "0.0.0.0");
        let partial = load_env_overrides();
        std::env::remove_var("LOOPER_SERVER_HOST");
        assert_eq!(partial.server.unwrap().host, Some("0.0.0.0".into()));
    }
}
