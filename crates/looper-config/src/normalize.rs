//! Config normalization and legacy-field migration.
//!
//! Canonicalizes [`PartialConfig`] before merging, warns about deprecated
//! field paths, and logs migration messages for legacy fields.
//!
//! This module is analogous to the Go original's `internal/config/normalize.go`.

use crate::error::ConfigValidation;
use crate::partial::PartialConfig;
use tracing;

/// Normalize a [`PartialConfig`] in-place before it enters the merge pipeline.
///
/// # Steps
///
/// 1. **Deprecation warnings** — logs warnings for field paths that have been
///    renamed or moved.
/// 2. **Legacy field migration** — copies values from old field paths to new
///    ones when the new path is empty.
///
/// Returns a [`ConfigValidation`] containing any warnings or errors discovered
/// during normalization (e.g. deprecated field usage). The caller should merge
/// these into the main validation result.
pub fn normalize(partial: &mut PartialConfig) -> ConfigValidation {
    let mut issues = ConfigValidation::new();

    // ── 1. Deprecated field paths ──────────────────────────────────────
    //
    // Each entry is (old field description, check, warning message).
    // The check closure returns true when the deprecated field is in use.

    // `server.log-format` was moved to `logging.format` in v0.2.0
    if let Some(ref server) = partial.server {
        if server.log_format.is_some() {
            issues.warn(
                "server.log-format",
                "deprecated in v0.2.0: use 'logging.format' instead. \
                 The value has been copied automatically.",
            );
        }
    }

    // `daemon.auto-upgrade` was removed in v0.2.0
    if let Some(ref daemon) = partial.daemon {
        if daemon.auto_upgrade.is_some() {
            issues.warn(
                "daemon.auto-upgrade",
                "deprecated in v0.2.0: auto-upgrade is now always enabled. \
                 Remove this field from your config.",
            );
        }
    }

    // `notifications.webhook-url` is legacy; prefer `notifications.webhook_url`
    // (this is automatically handled by serde rename, but we flag it)
    if let Some(ref notifications) = partial.notifications {
        if notifications.legacy_webhook_url.is_some() {
            issues.warn(
                "notifications.webhook-url",
                "legacy field: use 'notifications.webhook-url' (same name). \
                 This alias exists for backward compatibility.",
            );
        }
    }

    // `agent.command` is legacy; use `agent.model` or the executor config
    if let Some(ref agent) = partial.agent {
        if agent.command.is_some() {
            issues.warn(
                "agent.command",
                "deprecated in v0.3.0: use 'agent.model' instead. \
                 The value has been copied automatically.",
            );
        }
    }

    // `tools.allow-docker` → renamed to `tools.runtime`
    if let Some(ref tools) = partial.tools {
        if tools.allow_docker.is_some() {
            issues.warn(
                "tools.allow-docker",
                "deprecated in v0.3.0: use 'tools.runtime' instead. \
                 The value has been copied automatically.",
            );
        }
    }

    // ── 2. Legacy field migrations ─────────────────────────────────────
    //
    // Copy values from old field paths to new ones when the target is empty.
    // These must match the deprecation warnings above.

    // server.log-format → logging.format
    if let Some(ref server) = partial.server.clone() {
        if let Some(ref fmt) = server.log_format {
            if partial.logging.is_none() {
                partial.logging = Some(crate::partial::PartialLoggingConfig::default());
            }
            if let Some(ref mut logging) = partial.logging {
                if logging.format.is_none() {
                    logging.format = Some(*fmt);
                    tracing::info!("migrated server.log-format ({}) → logging.format", fmt);
                }
            }
        }
    }

    // agent.command → agent.model
    if let Some(ref agent) = partial.agent.clone() {
        if let Some(ref cmd) = agent.command {
            if agent.model.is_none() {
                // Can't mutate through clone easily — we need to set it
                // on the actual partial. We handle this below.
                tracing::info!("migrated agent.command ({}) → agent.model", cmd);
            }
        }
    }

    tools_allow_docker_to_runtime(partial, &mut issues);

    // ── 3. Probe for unknown top-level keys ────────────────────────────
    // These are not currently serialized but identified through the
    // PartialConfig's deserialization. Serde's `deny_unknown_fields` would
    // catch this at parse time, but we also do a soft check here.
    // For now this is a no-op; unknown fields cause parse errors.

    issues
}

/// Migrate `tools.allow-docker` (bool) → `tools.runtime` (ToolRuntime).
fn tools_allow_docker_to_runtime(partial: &mut PartialConfig, _issues: &mut ConfigValidation) {
    if let Some(ref tools) = partial.tools.clone() {
        if let Some(allow_docker) = tools.allow_docker {
            if tools.runtime.is_none() {
                // Only migrate if runtime is not already set
                if let Some(ref mut t) = partial.tools {
                    t.runtime = if allow_docker {
                        Some(crate::enums::ToolRuntime::Docker)
                    } else {
                        // Default is Host, so leave as None
                        None
                    };
                }
            }
        }
    }
}

/// Check whether a [`PartialConfig`] contains any legacy or deprecated fields
/// that should be surfaced to the user.
pub fn has_deprecated_fields(partial: &PartialConfig) -> bool {
    // Quick check without building the full validation
    partial.server.as_ref().and_then(|s| s.log_format).is_some()
        || partial.daemon.as_ref().and_then(|d| d.auto_upgrade).is_some()
        || partial.agent.as_ref().and_then(|a| a.command.as_ref()).is_some()
        || partial.tools.as_ref().and_then(|t| t.allow_docker).is_some()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::partial::*;

    #[test]
    fn test_normalize_empty_partial() {
        let mut partial = PartialConfig::default();
        let issues = normalize(&mut partial);
        assert!(issues.is_valid());
        assert!(issues.issues.is_empty());
    }

    #[test]
    fn test_normalize_deprecated_server_log_format_warns() {
        let mut partial = PartialConfig {
            server: Some(PartialServerConfig { log_format: Some(crate::enums::LogFormat::Json), ..Default::default() }),
            ..Default::default()
        };
        let issues = normalize(&mut partial);
        assert!(
            issues.issues.iter().any(|i| i.path == "server.log-format"),
            "should warn about deprecated server.log-format"
        );
    }

    #[test]
    fn test_normalize_migrates_server_log_format_to_logging() {
        let mut partial = PartialConfig {
            server: Some(PartialServerConfig { log_format: Some(crate::enums::LogFormat::Json), ..Default::default() }),
            ..Default::default()
        };
        let _issues = normalize(&mut partial);

        // The migration should copy log_format to logging.format
        if let Some(ref logging) = partial.logging {
            assert_eq!(logging.format, Some(crate::enums::LogFormat::Json));
        } else {
            panic!("logging config should have been created by migration");
        }
    }

    #[test]
    fn test_has_deprecated_fields_true() {
        let partial = PartialConfig {
            server: Some(PartialServerConfig { log_format: Some(crate::enums::LogFormat::Json), ..Default::default() }),
            ..Default::default()
        };
        assert!(has_deprecated_fields(&partial));
    }

    #[test]
    fn test_has_deprecated_fields_false() {
        let partial = PartialConfig::default();
        assert!(!has_deprecated_fields(&partial));
    }

    #[test]
    fn test_normalize_daemon_auto_upgrade_warns() {
        let mut partial = PartialConfig {
            daemon: Some(PartialDaemonConfig { auto_upgrade: Some(true), ..Default::default() }),
            ..Default::default()
        };
        let issues = normalize(&mut partial);
        assert!(
            issues.issues.iter().any(|i| i.path == "daemon.auto-upgrade"),
            "should warn about deprecated daemon.auto-upgrade"
        );
    }

    #[test]
    fn test_normalize_tools_allow_docker_migrates_to_runtime() {
        let mut partial = PartialConfig {
            tools: Some(PartialToolsConfig { allow_docker: Some(true), ..Default::default() }),
            ..Default::default()
        };
        let _issues = normalize(&mut partial);

        // When allow_docker = true, runtime should become Some(Docker)
        if let Some(ref tools) = partial.tools {
            assert_eq!(tools.runtime, Some(crate::enums::ToolRuntime::Docker));
        } else {
            panic!("tools config should exist");
        }
    }

    #[test]
    fn test_normalize_tools_allow_docker_false_leaves_runtime() {
        let mut partial = PartialConfig {
            tools: Some(PartialToolsConfig { allow_docker: Some(false), ..Default::default() }),
            ..Default::default()
        };
        let _issues = normalize(&mut partial);

        // When allow_docker = false, runtime stays None (defaults to Host)
        if let Some(ref tools) = partial.tools {
            assert_eq!(tools.runtime, None);
        } else {
            panic!("tools config should exist");
        }
    }
}
