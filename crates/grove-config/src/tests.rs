#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use camino::Utf8PathBuf;
use grove_types::{ReactionAction, ReactionTrigger, RuntimeProvider};
use std::{collections::HashMap, error::Error, fs, io::Error as IoError};
use tempfile::tempdir;

type TestResult = Result<(), Box<dyn Error>>;

#[test]
fn default_init_grove_toml_parses_to_default_config() -> TestResult {
    let parsed: GroveConfig =
        toml::from_str(DEFAULT_INIT_GROVE_TOML).map_err(|e| IoError::other(e.to_string()))?;
    assert_eq!(parsed, GroveConfig::default());
    Ok(())
}

#[test]
fn defaults_are_valid() -> TestResult {
    let dir = tempdir()?;
    let workspace_root = utf8(dir.path())?;
    let config_path = workspace_root.join("grove.toml");
    fs::write(&config_path, "")?;
    let loaded = load_from_path_with_env(&config_path, &HashMap::new())?;
    assert_eq!(loaded.config, GroveConfig::default());
    Ok(())
}

#[test]
fn env_override_sets_shutdown_grace_period() -> TestResult {
    let loaded = load_with_text(
        "",
        &HashMap::from([(
            String::from("GROVE_SCHEDULER__SHUTDOWN_GRACE_PERIOD_MS"),
            String::from("2500"),
        )]),
    )?;
    assert_eq!(loaded.config.scheduler.shutdown_grace_period_ms, 2500);
    Ok(())
}

#[test]
fn env_provider_override_resets_provider_bin_to_provider_default() -> TestResult {
    let loaded = load_with_text(
        "[runtime]\nprovider_bin = \"claude-custom\"\n",
        &HashMap::from([(
            String::from("GROVE_RUNTIME__PROVIDER"),
            String::from("codex"),
        )]),
    )?;
    assert_eq!(loaded.config.runtime.provider, RuntimeProvider::Codex);
    assert_eq!(loaded.config.runtime.provider_bin, "codex");
    assert_eq!(
        loaded.config.runtime.effective_init_args(),
        vec!["exec".to_owned(), "--full-auto".to_owned()]
    );
    Ok(())
}

#[test]
fn env_provider_bin_override_wins_over_provider_default() -> TestResult {
    let loaded = load_with_text(
        "",
        &HashMap::from([
            (
                String::from("GROVE_RUNTIME__PROVIDER"),
                String::from("codex"),
            ),
            (
                String::from("GROVE_RUNTIME__PROVIDER_BIN"),
                String::from("/tmp/custom-codex"),
            ),
        ]),
    )?;
    assert_eq!(loaded.config.runtime.provider, RuntimeProvider::Codex);
    assert_eq!(loaded.config.runtime.provider_bin, "/tmp/custom-codex");
    Ok(())
}

#[test]
fn legacy_startflag_alias_still_deserializes() -> TestResult {
    let loaded = load_with_text(
        "[runtime]\nstartflag = [\"--dangerously-skip-permissions\"]\n",
        &HashMap::new(),
    )?;
    assert_eq!(
        loaded.config.runtime.effective_init_args(),
        vec!["--dangerously-skip-permissions".to_owned()]
    );
    Ok(())
}

#[test]
fn env_init_args_override_wins_over_provider_defaults() -> TestResult {
    let loaded = load_with_text(
        "",
        &HashMap::from([
            (
                String::from("GROVE_RUNTIME__PROVIDER"),
                String::from("codex"),
            ),
            (
                String::from("GROVE_RUNTIME__INIT_ARGS"),
                String::from("exec,--ask-for-approval=never"),
            ),
        ]),
    )?;
    assert_eq!(loaded.config.runtime.provider, RuntimeProvider::Codex);
    assert_eq!(
        loaded.config.runtime.effective_init_args(),
        vec!["exec".to_owned(), "--ask-for-approval=never".to_owned()]
    );
    Ok(())
}

#[test]
fn legacy_claude_bin_is_ignored_when_provider_switches_to_codex() -> TestResult {
    let loaded = load_with_text(
        "",
        &HashMap::from([
            (
                String::from("GROVE_RUNTIME__PROVIDER"),
                String::from("codex"),
            ),
            (
                String::from("GROVE_RUNTIME__CLAUDE_BIN"),
                String::from("/tmp/custom-claude"),
            ),
        ]),
    )?;
    assert_eq!(loaded.config.runtime.provider, RuntimeProvider::Codex);
    assert_eq!(loaded.config.runtime.provider_bin, "codex");
    Ok(())
}

#[test]
fn env_override_sets_reaction_rules() -> TestResult {
    let loaded = load_with_text(
        "",
        &HashMap::from([(
            String::from("GROVE_REACTIONS__RULES"),
            String::from(
                r#"[{ trigger = "MirrorFailed", action = "EnqueueMirrorRetry", enabled = true, max_attempts = 7 }]"#,
            ),
        )]),
    )?;
    assert_eq!(loaded.config.reactions.rules.len(), 1);
    assert!(matches!(
        loaded.config.reactions.rules[0].trigger,
        ReactionTrigger::MirrorFailed
    ));
    assert!(matches!(
        loaded.config.reactions.rules[0].action,
        ReactionAction::EnqueueMirrorRetry
    ));
    assert_eq!(loaded.config.reactions.rules[0].max_attempts, 7);
    Ok(())
}

#[test]
fn env_override_rejects_invalid_reaction_rules() {
    let err = load_with_text(
        "",
        &HashMap::from([(
            String::from("GROVE_REACTIONS__RULES"),
            String::from(r#"[{ trigger = "NoProgress", action = { RetryWithMutation = { strategy = "NotARealStrategy" } }, enabled = true, max_attempts = 1 }]"#),
        )]),
    )
    .expect_err("expected validation error");
    assert!(
        matches!(err, ConfigError::Validation { field, .. } if field == "GROVE_REACTIONS__RULES")
    );
}

#[test]
fn load_minimal_toml() -> TestResult {
    let dir = tempdir()?;
    let workspace_root = utf8(dir.path())?;
    let config_path = workspace_root.join("grove.toml");
    fs::write(&config_path, "[runtime]\nworkspace_root = \".\"\n")?;

    let loaded = load_from_path_with_env(&config_path, &HashMap::new())?;
    assert_eq!(loaded.config.runtime.workspace_root, ".");
    assert_eq!(loaded.config.memory.db_path, DEFAULT_DB_PATH);
    assert_eq!(loaded.paths.workspace_root(), workspace_root.as_path());
    Ok(())
}

#[test]
fn load_full_toml() -> TestResult {
    let dir = tempdir()?;
    let workspace_root = utf8(dir.path())?;
    let config_path = workspace_root.join("grove.toml");
    fs::write(
        &config_path,
        r#"
[runtime]
provider_bin = "claude-custom"
default_model = "opus"
workspace_root = "."
timeout_minutes = 90
env_passthrough = ["FOO"]

[scheduler]
max_parallel = 7
poll_interval_ms = 500
shutdown_grace_period_ms = 1500
retry_max = 5
retry_backoff_secs = 45
critical_path_bonus = 30
ready_age_bonus_per_min = 2
retry_penalty = 3
reservation_conflict_penalty = 700

[checkpoint]
warn_pct = 0.5
rotate_pct = 0.7
hard_stop_pct = 0.9
max_context_bytes = 32000

[exit_policy]
completion_indicator_threshold = 3
heuristic_window = 10
require_explicit_exit = false

[circuit_breaker]
no_progress_threshold = 4
same_error_threshold = 6
permission_denial_threshold = 3
cooldown_minutes = 40

[memory]
db_path = ".grove/custom.db"
transcript_dir = ".grove/tlog"
archive_top_k = 8
max_prompt_snippets = 4
max_prompt_bullets = 10
enable_playbook = false
semantic_enabled = true

[reservations]
enabled = false
default_ttl_minutes = 30

[safety]
scan_transcripts = false
inject_safety_preamble = false

[logging]
level = "debug"
persist_jsonl = false
"#,
    )?;

    let loaded = load_from_path_with_env(&config_path, &HashMap::new())?;
    assert_eq!(loaded.config.runtime.provider_bin, "claude-custom");
    assert_eq!(loaded.config.scheduler.max_parallel, 7);
    assert_eq!(loaded.config.scheduler.shutdown_grace_period_ms, 1500);
    assert_eq!(loaded.config.checkpoint.max_context_bytes, 32000);
    assert_eq!(loaded.config.memory.transcript_dir, ".grove/tlog");
    assert_eq!(loaded.config.reactions.rules.len(), 6);
    assert_eq!(loaded.config.logging.level, "debug");
    Ok(())
}

#[test]
fn validation_rotate_gt_warn() -> TestResult {
    let err = load_with_text(
        "[checkpoint]\nwarn_pct = 0.8\nrotate_pct = 0.8\n",
        &HashMap::new(),
    )
    .err()
    .ok_or_else(|| IoError::other("expected validation error"))?;
    assert!(
        matches!(err, ConfigError::Validation { field, .. } if field == "checkpoint.rotate_pct")
    );
    Ok(())
}

#[test]
fn validation_hard_stop_gte_rotate() -> TestResult {
    let err = load_with_text(
        "[checkpoint]\nrotate_pct = 0.9\nhard_stop_pct = 0.8\n",
        &HashMap::new(),
    )
    .err()
    .ok_or_else(|| IoError::other("expected validation error"))?;
    assert!(
        matches!(err, ConfigError::Validation { field, .. } if field == "checkpoint.hard_stop_pct")
    );
    Ok(())
}

#[test]
fn validation_max_parallel_gte_1() -> TestResult {
    let err = load_with_text("[scheduler]\nmax_parallel = 0\n", &HashMap::new())
        .err()
        .ok_or_else(|| IoError::other("expected validation error"))?;
    assert!(
        matches!(err, ConfigError::Validation { field, .. } if field == "scheduler.max_parallel")
    );
    Ok(())
}

#[test]
fn validation_retry_max_gte_1() -> TestResult {
    let err = load_with_text("[scheduler]\nretry_max = 0\n", &HashMap::new())
        .err()
        .ok_or_else(|| IoError::other("expected validation error"))?;
    assert!(matches!(err, ConfigError::Validation { field, .. } if field == "scheduler.retry_max"));
    Ok(())
}

#[test]
fn validation_percentages_in_range() -> TestResult {
    let err = load_with_text("[checkpoint]\nwarn_pct = 1.2\n", &HashMap::new())
        .err()
        .ok_or_else(|| IoError::other("expected validation error"))?;
    assert!(matches!(err, ConfigError::Validation { field, .. } if field == "checkpoint.warn_pct"));
    Ok(())
}

#[test]
fn validation_completion_threshold_gte_1() -> TestResult {
    let err = load_with_text(
        "[exit_policy]\ncompletion_indicator_threshold = 0\n",
        &HashMap::new(),
    )
    .err()
    .ok_or_else(|| IoError::other("expected validation error"))?;
    assert!(
        matches!(err, ConfigError::Validation { field, .. } if field == "exit_policy.completion_indicator_threshold")
    );
    Ok(())
}

#[test]
fn validation_cooldown_minutes_gte_1() -> TestResult {
    let err = load_with_text("[circuit_breaker]\ncooldown_minutes = 0\n", &HashMap::new())
        .err()
        .ok_or_else(|| IoError::other("expected validation error"))?;
    assert!(
        matches!(err, ConfigError::Validation { field, .. } if field == "circuit_breaker.cooldown_minutes")
    );
    Ok(())
}

#[test]
fn relative_path_resolution() -> TestResult {
    let loaded = load_with_text(
        "[memory]\ndb_path = \"data/grove.db\"\ntranscript_dir = \"data/transcripts\"\n",
        &HashMap::new(),
    )?;
    assert!(loaded.paths.db_path().as_str().ends_with("data/grove.db"));
    assert!(
        loaded
            .paths
            .transcript_dir()
            .as_str()
            .ends_with("data/transcripts")
    );
    Ok(())
}

#[test]
#[ignore = "Windows absolute paths need TOML escaping in this fixture"]
fn absolute_path_preserved() -> TestResult {
    let dir = tempdir()?;
    let workspace_root = utf8(dir.path())?;
    let absolute_db = workspace_root.join("outside.db");
    let text = format!("[memory]\ndb_path = \"{}\"\n", absolute_db.as_str());
    let loaded = load_with_text(&text, &HashMap::new())?;
    assert_eq!(loaded.paths.db_path(), absolute_db.as_path());
    Ok(())
}

#[test]
fn missing_file_returns_error() -> TestResult {
    let dir = tempdir()?;
    let workspace_root = utf8(dir.path())?;
    let missing = workspace_root.join("missing.toml");
    let err = load_from_path_with_env(&missing, &HashMap::new())
        .err()
        .ok_or_else(|| IoError::other("expected missing file error"))?;
    assert!(matches!(err, ConfigError::FileNotFound { .. }));
    Ok(())
}

#[test]
fn invalid_toml_returns_parse_error() -> TestResult {
    let err = load_with_text("[runtime\nclaude_bin = \"claude\"\n", &HashMap::new())
        .err()
        .ok_or_else(|| IoError::other("expected parse error"))?;
    assert!(matches!(err, ConfigError::TomlParse { .. }));
    Ok(())
}

#[test]
fn env_override_applies() -> TestResult {
    let mut env = HashMap::new();
    env.insert(
        "GROVE_RUNTIME__TIMEOUT_MINUTES".to_owned(),
        "120".to_owned(),
    );
    env.insert(
        "GROVE_MEMORY__ENABLE_PLAYBOOK".to_owned(),
        "false".to_owned(),
    );
    let loaded = load_with_text("", &env)?;
    assert_eq!(loaded.config.runtime.timeout_minutes, 120);
    assert!(!loaded.config.memory.enable_playbook);
    Ok(())
}

#[test]
fn conflicting_paths_detected() -> TestResult {
    let err = load_with_text(
        "[memory]\ndb_path = \".grove/conflict\"\ntranscript_dir = \".grove/conflict\"\n",
        &HashMap::new(),
    )
    .err()
    .ok_or_else(|| IoError::other("expected conflict error"))?;
    assert!(matches!(err, ConfigError::Conflict { .. }));
    Ok(())
}

fn load_with_text(text: &str, env: &HashMap<String, String>) -> Result<LoadedConfig, ConfigError> {
    let dir = tempdir().map_err(|source| ConfigError::Validation {
        field: "test.tempdir".to_owned(),
        message: source.to_string(),
    })?;
    let workspace_root = utf8(dir.path()).map_err(|source| ConfigError::Validation {
        field: "test.workspace_root".to_owned(),
        message: source.to_string(),
    })?;
    let config_path = workspace_root.join("grove.toml");
    fs::write(&config_path, text).map_err(|source| ConfigError::Validation {
        field: "test.config_write".to_owned(),
        message: source.to_string(),
    })?;
    load_from_path_with_env(&config_path, env)
}

fn utf8(path: &std::path::Path) -> Result<Utf8PathBuf, IoError> {
    Utf8PathBuf::from_path_buf(path.to_path_buf())
        .map_err(|_| IoError::other("temporary path was not valid UTF-8"))
}
