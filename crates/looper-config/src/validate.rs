use crate::error::ConfigValidation;
use crate::types::*;

/// Run all validation rules against the resolved config.
///
/// Returns a `ConfigValidation` that collects all issues (errors + warnings).
/// The caller should check `has_errors()` before accepting the config.
pub fn validate_config(config: &Config) -> ConfigValidation {
    let mut issues = ConfigValidation::new();

    // -- Server --
    if let Some(ref server) = config.server {
        if server.port == 0 {
            issues.error("server.port", "port must not be 0");
        }
        if server.host.is_empty() {
            issues.error("server.host", "host must not be empty");
        }
        if server.max_body_size_mb == 0 {
            issues.warn("server.max-body-size-mb", "max body size is 0 (uploads disabled)");
        }
    }

    // -- Daemon --
    if let Some(ref daemon) = config.daemon {
        if daemon.max_restarts == 0 {
            issues.warn("daemon.max-restarts", "max restarts is 0 (daemon won't restart)");
        }
    }

    // -- Storage --
    if let Some(ref storage) = config.storage {
        if storage.pool_size == 0 {
            issues.error("storage.pool-size", "pool size must be >= 1");
        }
        if storage.timeout_secs == 0 {
            issues.warn("storage.timeout-secs", "timeout is 0 (may hang on busy db)");
        }
    }

    // -- Scheduler --
    if let Some(ref scheduler) = config.scheduler {
        if scheduler.max_workers == 0 {
            issues.error("scheduler.max-workers", "at least 1 worker required");
        }
        if scheduler.queue_capacity == 0 {
            issues.error("scheduler.queue-capacity", "queue capacity must be >= 1");
        }
        if scheduler.poll_interval_ms == 0 {
            issues.warn("scheduler.poll-interval-ms", "poll interval is 0 (busy loop)");
        }
        if scheduler.retry.max_attempts == 0 {
            issues.warn("scheduler.retry.max-attempts", "max attempts is 0 (retries disabled)");
        }
        if scheduler.retry.base_delay_secs == 0 {
            issues.warn("scheduler.retry.base-delay-secs", "base delay is 0 (may cause tight retry loop)");
        }
    }

    // -- Agent --
    if let Some(ref agent) = config.agent {
        if agent.timeout_secs == 0 {
            issues.error("agent.timeout-secs", "timeout must be > 0");
        }
        if agent.parallel_runs == 0 {
            issues.warn("agent.parallel-runs", "parallel runs is 0 (agent won't execute)");
        }
    }

    // -- Logging --
    if let Some(ref logging) = config.logging {
        if logging.max_files == 0 && logging.output == crate::enums::LogOutput::File {
            issues.warn("logging.max-files", "max files is 0 with file output (unlimited growth)");
        }
        if logging.max_size_mb == 0 {
            issues.warn("logging.max-size-mb", "max log size is 0 (unbounded)");
        }
    }

    // -- Notifications --
    if let Some(ref notifications) = config.notifications {
        if notifications.enabled && notifications.channels.is_empty() {
            issues.warn("notifications.channels", "notifications enabled but no channels configured");
        }
    }

    // -- Disclosure --
    if let Some(ref disclosure) = config.disclosure {
        if disclosure.max_length.is_some_and(|l| l == 0) {
            issues.warn("disclosure.max-length", "max length is 0 (all disclosure suppressed)");
        }
    }

    // -- Tools --
    if let Some(ref tools) = config.tools {
        if tools.timeout_secs == 0 {
            issues.error("tools.timeout-secs", "tool timeout must be > 0");
        }
    }

    // -- Instructions --
    if let Some(ref instructions) = config.instructions {
        if instructions.max_size_kb == 0 {
            issues.warn("instructions.max-size-kb", "max instruction size is 0 (all instructions rejected)");
        }
    }

    // -- Roles --
    if let Some(ref roles) = config.roles {
        validate_role(&roles.planner, "roles.planner", &mut issues);
        validate_role(&roles.reviewer, "roles.reviewer", &mut issues);
        validate_role(&roles.worker, "roles.worker", &mut issues);
        validate_role(&roles.fixer, "roles.fixer", &mut issues);
        validate_role(&roles.coordinator, "roles.coordinator", &mut issues);
    }

    // -- Projects --
    for (i, project) in config.projects.iter().enumerate() {
        let prefix = format!("projects[{}]", i);
        if project.name.is_empty() {
            issues.error(format!("{}.name", prefix), "project name must not be empty");
        }
    }

    issues
}

fn validate_role(role: &RoleConfig, path: &str, issues: &mut ConfigValidation) {
    if role.enabled && role.timeout_secs == 0 {
        issues.warn(format!("{}.timeout-secs", path), "enabled role has 0 timeout");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    

    fn minimal_valid_config() -> Config {
        Config {
            server: Some(ServerConfig { host: "127.0.0.1".into(), port: 7391, ..Default::default() }),
            ..Default::default()
        }
    }

    #[test]
    fn test_valid_minimal_config() {
        let config = minimal_valid_config();
        let validation = validate_config(&config);
        assert!(!validation.has_errors(), "minimal config should pass: {:?}", validation.issues);
    }

    #[test]
    fn test_zero_port_is_error() {
        let mut config = minimal_valid_config();
        if let Some(ref mut server) = config.server {
            server.port = 0;
        }
        let validation = validate_config(&config);
        assert!(validation.has_errors());
        assert!(validation.issues.iter().any(|i| i.path == "server.port"));
    }

    #[test]
    fn test_empty_host_is_error() {
        let mut config = minimal_valid_config();
        if let Some(ref mut server) = config.server {
            server.host = String::new();
        }
        let validation = validate_config(&config);
        assert!(validation.has_errors());
    }

    #[test]
    fn test_zero_workers_is_error() {
        let mut config = minimal_valid_config();
        config.scheduler = Some(SchedulerConfig { max_workers: 0, ..Default::default() });
        let validation = validate_config(&config);
        assert!(validation.has_errors());
    }

    #[test]
    fn test_zero_timeout_is_error() {
        let mut config = minimal_valid_config();
        config.agent = Some(AgentConfig { timeout_secs: 0, ..Default::default() });
        let validation = validate_config(&config);
        assert!(validation.has_errors());
    }

    #[test]
    fn test_empty_project_name_is_error() {
        let mut config = minimal_valid_config();
        config.projects = vec![ProjectConfig { name: String::new(), ..Default::default() }];
        let validation = validate_config(&config);
        assert!(validation.has_errors());
    }

    #[test]
    fn test_warnings_do_not_cause_has_errors() {
        let mut config = minimal_valid_config();
        config.agent = Some(AgentConfig { parallel_runs: 0, ..Default::default() });
        let validation = validate_config(&config);
        assert!(!validation.has_errors(), "warnings should not cause errors");
    }
}
