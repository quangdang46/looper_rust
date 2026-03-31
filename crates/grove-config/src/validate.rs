use crate::{ConfigError, GroveConfig, GrovePaths};

pub fn validate_config(config: &GroveConfig, paths: &GrovePaths) -> Result<(), ConfigError> {
    if config.runtime.provider_bin.trim().is_empty() {
        return Err(ConfigError::Validation {
            field: "runtime.provider_bin".to_owned(),
            message: "must not be empty".to_owned(),
        });
    }

    if let Some(init_args) = &config.runtime.init_args {
        for (index, flag) in init_args.iter().enumerate() {
            if flag.trim().is_empty() {
                return Err(ConfigError::Validation {
                    field: format!("runtime.init_args[{index}]"),
                    message: "must not be empty".to_owned(),
                });
            }
        }
    }

    ensure_range("checkpoint.warn_pct", config.checkpoint.warn_pct)?;
    ensure_range("checkpoint.rotate_pct", config.checkpoint.rotate_pct)?;
    ensure_range("checkpoint.hard_stop_pct", config.checkpoint.hard_stop_pct)?;

    if config.checkpoint.rotate_pct <= config.checkpoint.warn_pct {
        return Err(ConfigError::Validation {
            field: "checkpoint.rotate_pct".to_owned(),
            message: "must be greater than checkpoint.warn_pct".to_owned(),
        });
    }

    if config.checkpoint.hard_stop_pct < config.checkpoint.rotate_pct {
        return Err(ConfigError::Validation {
            field: "checkpoint.hard_stop_pct".to_owned(),
            message: "must be greater than or equal to checkpoint.rotate_pct".to_owned(),
        });
    }

    if config.scheduler.max_parallel < 1 {
        return Err(ConfigError::Validation {
            field: "scheduler.max_parallel".to_owned(),
            message: "must be at least 1".to_owned(),
        });
    }

    if config.scheduler.retry_max < 1 {
        return Err(ConfigError::Validation {
            field: "scheduler.retry_max".to_owned(),
            message: "must be at least 1".to_owned(),
        });
    }

    if config.exit_policy.completion_indicator_threshold < 1 {
        return Err(ConfigError::Validation {
            field: "exit_policy.completion_indicator_threshold".to_owned(),
            message: "must be at least 1".to_owned(),
        });
    }

    if config.circuit_breaker.cooldown_minutes < 1 {
        return Err(ConfigError::Validation {
            field: "circuit_breaker.cooldown_minutes".to_owned(),
            message: "must be at least 1".to_owned(),
        });
    }

    if config.runtime.timeout_minutes < 1 {
        return Err(ConfigError::Validation {
            field: "runtime.timeout_minutes".to_owned(),
            message: "must be at least 1".to_owned(),
        });
    }

    if config.scheduler.poll_interval_ms < 1 {
        return Err(ConfigError::Validation {
            field: "scheduler.poll_interval_ms".to_owned(),
            message: "must be at least 1".to_owned(),
        });
    }

    if config.checkpoint.max_context_bytes < 1 {
        return Err(ConfigError::Validation {
            field: "checkpoint.max_context_bytes".to_owned(),
            message: "must be at least 1".to_owned(),
        });
    }

    validate_path_collisions(paths)
}

fn ensure_range(field: &str, value: f32) -> Result<(), ConfigError> {
    if (0.0..=1.0).contains(&value) {
        Ok(())
    } else {
        Err(ConfigError::Validation {
            field: field.to_owned(),
            message: "must be within [0.0, 1.0]".to_owned(),
        })
    }
}

fn validate_path_collisions(paths: &GrovePaths) -> Result<(), ConfigError> {
    let managed = paths.managed_paths();
    for (index, (left_name, left_path)) in managed.iter().enumerate() {
        for (right_name, right_path) in managed.iter().skip(index + 1) {
            if left_path == right_path {
                return Err(ConfigError::Conflict {
                    a: (*left_name).to_owned(),
                    b: (*right_name).to_owned(),
                });
            }
        }
    }
    Ok(())
}
