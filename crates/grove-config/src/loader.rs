use std::{
    collections::{HashMap, HashSet},
    process::Command,
};

use camino::Utf8Path;
use grove_types::RuntimeProvider;

use crate::{ConfigError, GroveConfig, GrovePaths, RuntimeConfig, validate_config};

#[derive(Debug, Clone, PartialEq)]
pub struct LoadedConfig {
    pub config: GroveConfig,
    pub paths: GrovePaths,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCapability {
    pub binary: String,
    pub available: bool,
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredTooling {
    pub active_provider: ToolCapability,
    pub claude: ToolCapability,
    pub codex: ToolCapability,
    pub br: ToolCapability,
    pub bv: ToolCapability,
}

pub fn load_from_workspace(workspace_root: &Utf8Path) -> Result<LoadedConfig, ConfigError> {
    let path = workspace_root.join("grove.toml");
    load_from_path(&path)
}

pub fn load_from_path(path: &Utf8Path) -> Result<LoadedConfig, ConfigError> {
    let env = std::env::vars().collect::<HashMap<String, String>>();
    load_from_path_with_env(path, &env)
}

pub fn load_from_path_with_env(
    path: &Utf8Path,
    env: &HashMap<String, String>,
) -> Result<LoadedConfig, ConfigError> {
    if !path.exists() {
        return Err(ConfigError::FileNotFound {
            path: path.to_string(),
        });
    }

    let raw = std::fs::read_to_string(path).map_err(|source| ConfigError::Validation {
        field: "config_file".to_owned(),
        message: source.to_string(),
    })?;

    let mut config: GroveConfig =
        toml::from_str(&raw).map_err(|source| ConfigError::TomlParse { source })?;
    apply_env_overrides(&mut config, env)?;

    let paths = GrovePaths::from_config(&config, path)?;
    validate_config(&config, &paths)?;

    Ok(LoadedConfig { config, paths })
}

pub fn apply_env_overrides(
    config: &mut GroveConfig,
    env: &HashMap<String, String>,
) -> Result<(), ConfigError> {
    let mut provider_override = None;
    let mut provider_bin_override = None;
    let mut init_args_override = None;
    let mut legacy_claude_bin_override = None;

    for (key, value) in env {
        match key.as_str() {
            "GROVE_RUNTIME__PROVIDER" => provider_override = Some(parse_provider_env(key, value)?),
            "GROVE_RUNTIME__PROVIDER_BIN" => provider_bin_override = Some(value.clone()),
            "GROVE_RUNTIME__INIT_ARGS"
            | "GROVE_RUNTIME__INIT_FLAG"
            | "GROVE_RUNTIME__STARTFLAG" => init_args_override = Some(parse_csv(value)),
            "GROVE_RUNTIME__CLAUDE_BIN" => legacy_claude_bin_override = Some(value.clone()),
            "GROVE_RUNTIME__DEFAULT_MODEL" => config.runtime.default_model = value.clone(),
            "GROVE_RUNTIME__WORKSPACE_ROOT" => config.runtime.workspace_root = value.clone(),
            "GROVE_RUNTIME__TIMEOUT_MINUTES" => {
                config.runtime.timeout_minutes = parse_env(key, value)?
            }
            "GROVE_RUNTIME__ENV_PASSTHROUGH" => config.runtime.env_passthrough = parse_csv(value),
            "GROVE_SCHEDULER__MAX_PARALLEL" => {
                config.scheduler.max_parallel = parse_env(key, value)?
            }
            "GROVE_SCHEDULER__POLL_INTERVAL_MS" => {
                config.scheduler.poll_interval_ms = parse_env(key, value)?
            }
            "GROVE_SCHEDULER__SHUTDOWN_GRACE_PERIOD_MS" => {
                config.scheduler.shutdown_grace_period_ms = parse_env(key, value)?
            }
            "GROVE_SCHEDULER__RETRY_MAX" => config.scheduler.retry_max = parse_env(key, value)?,
            "GROVE_SCHEDULER__RETRY_BACKOFF_SECS" => {
                config.scheduler.retry_backoff_secs = parse_env(key, value)?
            }
            "GROVE_SCHEDULER__CRITICAL_PATH_BONUS" => {
                config.scheduler.critical_path_bonus = parse_env(key, value)?
            }
            "GROVE_SCHEDULER__READY_AGE_BONUS_PER_MIN" => {
                config.scheduler.ready_age_bonus_per_min = parse_env(key, value)?
            }
            "GROVE_SCHEDULER__RETRY_PENALTY" => {
                config.scheduler.retry_penalty = parse_env(key, value)?
            }
            "GROVE_SCHEDULER__RESERVATION_CONFLICT_PENALTY" => {
                config.scheduler.reservation_conflict_penalty = parse_env(key, value)?
            }
            "GROVE_CHECKPOINT__WARN_PCT" => config.checkpoint.warn_pct = parse_env(key, value)?,
            "GROVE_CHECKPOINT__ROTATE_PCT" => config.checkpoint.rotate_pct = parse_env(key, value)?,
            "GROVE_CHECKPOINT__HARD_STOP_PCT" => {
                config.checkpoint.hard_stop_pct = parse_env(key, value)?
            }
            "GROVE_CHECKPOINT__MAX_CONTEXT_BYTES" => {
                config.checkpoint.max_context_bytes = parse_env(key, value)?
            }
            "GROVE_EXIT_POLICY__COMPLETION_INDICATOR_THRESHOLD" => {
                config.exit_policy.completion_indicator_threshold = parse_env(key, value)?
            }
            "GROVE_EXIT_POLICY__HEURISTIC_WINDOW" => {
                config.exit_policy.heuristic_window = parse_env(key, value)?
            }
            "GROVE_EXIT_POLICY__REQUIRE_EXPLICIT_EXIT" => {
                config.exit_policy.require_explicit_exit = parse_env(key, value)?
            }
            "GROVE_CIRCUIT_BREAKER__NO_PROGRESS_THRESHOLD" => {
                config.circuit_breaker.no_progress_threshold = parse_env(key, value)?
            }
            "GROVE_CIRCUIT_BREAKER__SAME_ERROR_THRESHOLD" => {
                config.circuit_breaker.same_error_threshold = parse_env(key, value)?
            }
            "GROVE_CIRCUIT_BREAKER__PERMISSION_DENIAL_THRESHOLD" => {
                config.circuit_breaker.permission_denial_threshold = parse_env(key, value)?
            }
            "GROVE_CIRCUIT_BREAKER__COOLDOWN_MINUTES" => {
                config.circuit_breaker.cooldown_minutes = parse_env(key, value)?
            }
            "GROVE_MEMORY__DB_PATH" => config.memory.db_path = value.clone(),
            "GROVE_MEMORY__TRANSCRIPT_DIR" => config.memory.transcript_dir = value.clone(),
            "GROVE_MEMORY__ARCHIVE_TOP_K" => config.memory.archive_top_k = parse_env(key, value)?,
            "GROVE_MEMORY__MAX_PROMPT_SNIPPETS" => {
                config.memory.max_prompt_snippets = parse_env(key, value)?
            }
            "GROVE_MEMORY__MAX_PROMPT_BULLETS" => {
                config.memory.max_prompt_bullets = parse_env(key, value)?
            }
            "GROVE_MEMORY__ENABLE_PLAYBOOK" => {
                config.memory.enable_playbook = parse_env(key, value)?
            }
            "GROVE_MEMORY__SEMANTIC_ENABLED" => {
                config.memory.semantic_enabled = parse_env(key, value)?
            }
            "GROVE_RESERVATIONS__ENABLED" => config.reservations.enabled = parse_env(key, value)?,
            "GROVE_RESERVATIONS__DEFAULT_TTL_MINUTES" => {
                config.reservations.default_ttl_minutes = parse_env(key, value)?
            }
            "GROVE_REACTIONS__RULES" => config.reactions.rules = parse_reaction_rules(key, value)?,
            "GROVE_SAFETY__SCAN_TRANSCRIPTS" => {
                config.safety.scan_transcripts = parse_env(key, value)?
            }
            "GROVE_SAFETY__INJECT_SAFETY_PREAMBLE" => {
                config.safety.inject_safety_preamble = parse_env(key, value)?
            }
            "GROVE_LOGGING__LEVEL" => config.logging.level = value.clone(),
            "GROVE_LOGGING__PERSIST_JSONL" => config.logging.persist_jsonl = parse_env(key, value)?,
            _ => {}
        }
    }

    if let Some(provider) = provider_override {
        config.runtime.provider = provider;
        config.runtime.provider_bin = provider.default_bin().to_owned();
        config.runtime.init_args = Some(
            provider
                .default_init_args()
                .iter()
                .map(|flag| (*flag).to_owned())
                .collect(),
        );
    }

    if let Some(provider_bin) = provider_bin_override {
        config.runtime.provider_bin = provider_bin;
    } else if let Some(legacy_claude_bin) = legacy_claude_bin_override
        && matches!(config.runtime.provider, RuntimeProvider::Claude)
    {
        config.runtime.provider_bin = legacy_claude_bin;
    }

    if let Some(init_args) = init_args_override {
        config.runtime.init_args = Some(init_args);
    }

    Ok(())
}

pub fn build_provider_environment(
    provider: RuntimeProvider,
    config: &RuntimeConfig,
    current_env: &HashMap<String, String>,
) -> Vec<(String, String)> {
    let mut always_pass = vec![
        "HOME", "USER", "PATH", "SHELL", "TERM", "LANG", "LC_ALL", "TZ",
    ];
    match provider {
        RuntimeProvider::Claude => {
            always_pass.extend(["ANTHROPIC_API_KEY", "CLAUDE_API_KEY"]);
        }
        RuntimeProvider::Codex => {
            always_pass.extend(["OPENAI_API_KEY"]);
        }
    }
    let never_pass = [
        "AWS_SECRET_ACCESS_KEY",
        "GCP_SERVICE_ACCOUNT_KEY",
        "GITHUB_TOKEN",
        "NPM_TOKEN",
        "DOCKER_PASSWORD",
    ];

    let mut seen = HashSet::new();
    let mut env = Vec::new();

    for key in always_pass {
        if let Some(value) = current_env.get(key)
            && seen.insert(key.to_owned())
        {
            env.push((key.to_owned(), value.clone()));
        }
    }

    for key in &config.env_passthrough {
        if never_pass.contains(&key.as_str()) {
            continue;
        }
        if let Some(value) = current_env.get(key)
            && seen.insert(key.clone())
        {
            env.push((key.clone(), value.clone()));
        }
    }

    env
}

pub fn detect_required_tooling(config: &GroveConfig) -> RequiredTooling {
    let claude = detect_tool(RuntimeProvider::Claude.default_bin());
    let codex = detect_tool(RuntimeProvider::Codex.default_bin());
    let active_provider = detect_tool(&config.runtime.provider_bin);
    RequiredTooling {
        active_provider,
        claude,
        codex,
        br: detect_tool("br"),
        bv: detect_tool("bv"),
    }
}

fn parse_provider_env(key: &str, value: &str) -> Result<RuntimeProvider, ConfigError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "claude" => Ok(RuntimeProvider::Claude),
        "codex" => Ok(RuntimeProvider::Codex),
        _ => Err(ConfigError::Validation {
            field: key.to_owned(),
            message: format!("unsupported provider `{value}`; expected `claude` or `codex`"),
        }),
    }
}

fn detect_tool(binary: &str) -> ToolCapability {
    match Command::new(binary).arg("--version").output() {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            let version = first_non_empty_line(&stdout).or_else(|| first_non_empty_line(&stderr));
            ToolCapability {
                binary: binary.to_owned(),
                available: true,
                version,
            }
        }
        Err(_) => ToolCapability {
            binary: binary.to_owned(),
            available: false,
            version: None,
        },
    }
}

fn first_non_empty_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(ToOwned::to_owned)
}

fn parse_env<T>(field: &str, value: &str) -> Result<T, ConfigError>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    value
        .parse::<T>()
        .map_err(|source| ConfigError::Validation {
            field: field.to_owned(),
            message: source.to_string(),
        })
}

fn parse_csv(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn parse_reaction_rules(
    field: &str,
    value: &str,
) -> Result<Vec<grove_types::ReactionRule>, ConfigError> {
    #[derive(serde::Deserialize)]
    struct ReactionRulesEnvelope {
        rules: Vec<grove_types::ReactionRule>,
    }

    toml::from_str::<ReactionRulesEnvelope>(&format!("rules = {value}"))
        .map(|envelope| envelope.rules)
        .map_err(|source| ConfigError::Validation {
            field: field.to_owned(),
            message: source.to_string(),
        })
}
