use crate::defaults::{DEFAULT_DB_PATH, DEFAULT_STARTUP_PROMPT_PATH, DEFAULT_TRANSCRIPT_DIR};
use grove_types::{ReactionRule, default_reactions};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GroveConfig {
    pub runtime: RuntimeConfig,
    pub scheduler: SchedulerConfig,
    pub checkpoint: CheckpointConfig,
    pub exit_policy: ExitPolicyConfig,
    pub circuit_breaker: CircuitBreakerConfig,
    pub memory: MemoryConfig,
    pub reservations: ReservationConfig,
    pub reactions: ReactionConfig,
    pub safety: SafetyConfig,
    pub logging: LoggingConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RuntimeConfig {
    pub claude_bin: String,
    /// Use `"default"` to let the Claude CLI pick the model (Grove omits `--model`).
    pub default_model: String,
    pub workspace_root: String,
    pub timeout_minutes: u64,
    pub startup_prompt_path: String,
    pub env_passthrough: Vec<String>,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            claude_bin: "claude".to_owned(),
            default_model: "default".to_owned(),
            workspace_root: ".".to_owned(),
            timeout_minutes: 60,
            startup_prompt_path: DEFAULT_STARTUP_PROMPT_PATH.to_owned(),
            env_passthrough: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SchedulerConfig {
    pub max_parallel: usize,
    pub poll_interval_ms: u64,
    pub shutdown_grace_period_ms: u64,
    pub retry_max: u32,
    pub retry_backoff_secs: u64,
    pub critical_path_bonus: i32,
    pub ready_age_bonus_per_min: i32,
    pub retry_penalty: i32,
    pub reservation_conflict_penalty: i32,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            max_parallel: 5,
            poll_interval_ms: 1000,
            shutdown_grace_period_ms: 1000,
            retry_max: 3,
            retry_backoff_secs: 30,
            critical_path_bonus: 20,
            ready_age_bonus_per_min: 1,
            retry_penalty: 10,
            reservation_conflict_penalty: 1000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CheckpointConfig {
    pub warn_pct: f32,
    pub rotate_pct: f32,
    pub hard_stop_pct: f32,
    pub max_context_bytes: usize,
}

impl Default for CheckpointConfig {
    fn default() -> Self {
        Self {
            warn_pct: 0.70,
            rotate_pct: 0.82,
            hard_stop_pct: 0.90,
            max_context_bytes: 16_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExitPolicyConfig {
    pub completion_indicator_threshold: u32,
    pub heuristic_window: usize,
    pub require_explicit_exit: bool,
}

impl Default for ExitPolicyConfig {
    fn default() -> Self {
        Self {
            completion_indicator_threshold: 2,
            heuristic_window: 8,
            require_explicit_exit: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CircuitBreakerConfig {
    pub no_progress_threshold: u32,
    pub same_error_threshold: u32,
    pub permission_denial_threshold: u32,
    pub cooldown_minutes: u64,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            no_progress_threshold: 3,
            same_error_threshold: 5,
            permission_denial_threshold: 2,
            cooldown_minutes: 30,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MemoryConfig {
    pub db_path: String,
    pub transcript_dir: String,
    pub archive_top_k: usize,
    pub max_prompt_snippets: usize,
    pub max_prompt_bullets: usize,
    pub enable_playbook: bool,
    pub semantic_enabled: bool,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            db_path: DEFAULT_DB_PATH.to_owned(),
            transcript_dir: DEFAULT_TRANSCRIPT_DIR.to_owned(),
            archive_top_k: 5,
            max_prompt_snippets: 3,
            max_prompt_bullets: 12,
            enable_playbook: true,
            semantic_enabled: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReservationConfig {
    pub enabled: bool,
    pub default_ttl_minutes: u64,
}

impl Default for ReservationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            default_ttl_minutes: 60,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReactionConfig {
    pub rules: Vec<ReactionRule>,
}

impl Default for ReactionConfig {
    fn default() -> Self {
        Self {
            rules: default_reactions(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SafetyConfig {
    pub scan_transcripts: bool,
    pub inject_safety_preamble: bool,
}

impl Default for SafetyConfig {
    fn default() -> Self {
        Self {
            scan_transcripts: true,
            inject_safety_preamble: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LoggingConfig {
    pub level: String,
    pub persist_jsonl: bool,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".to_owned(),
            persist_jsonl: true,
        }
    }
}
