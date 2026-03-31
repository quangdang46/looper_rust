//! Default `grove.toml` text for `grove init` (includes comments; serde defaults fill omitted keys).

/// TOML written when `grove init` creates a new config file.
///
/// Omitted sections/keys use the same defaults as [`crate::GroveConfig::default()`] (for example
/// `[reactions]`, `memory.db_path`, `scheduler.shutdown_grace_period_ms`).
pub const DEFAULT_INIT_GROVE_TOML: &str = r#"# Grove orchestration configuration (created by `grove init`).
# See the grove README "Config" section for full documentation.

[runtime]
# Runtime provider used for Grove task sessions: `claude` or `codex`.
provider = "claude"
# Executable name or path for the selected provider CLI (must be on PATH unless you use an absolute path).
provider_bin = "claude"
# Provider startup args/subcommands prepended before Grove-managed prompt/model arguments.
init_args = ["--dangerously-skip-permissions"]
# Model passed to the selected provider. Use "default" to omit the model flag and let the CLI pick.
default_model = "default"
# Directory Grove treats as the project root (relative to this file, or absolute).
workspace_root = "."
# Kill the session if it runs longer than this many minutes.
timeout_minutes = 60
# Workspace-local Markdown prepended to every Grove session prompt.
startup_prompt_path = ".grove/startup_prompt.md"

[scheduler]
# Maximum concurrent provider sessions (still subject to reservation safety).
max_parallel = 5
# How often the scheduler wakes to check for ready work (milliseconds).
poll_interval_ms = 1000
# How many times to retry a failed dispatch before giving up on that attempt.
retry_max = 3
# Wait this many seconds between retry attempts (backoff base).
retry_backoff_secs = 30
# Score bonus for tasks on the critical path (higher = scheduled sooner).
critical_path_bonus = 20
# Penalty score when a bead’s claimed paths overlap another reservation (higher = stronger deprioritization).
reservation_conflict_penalty = 1000

[checkpoint]
# Fraction of context at which Grove warns that the session is under pressure (0.0–1.0).
warn_pct = 0.70
# Fraction of context at which Grove should prompt a checkpoint / rotation (0.0–1.0).
rotate_pct = 0.82
# Fraction of context at which Grove treats context as critically full (0.0–1.0).
hard_stop_pct = 0.90

[exit_policy]
# How many “completion-like” stdout signals count toward treating the task as done.
completion_indicator_threshold = 2
# How many recent lines of output the exit heuristic considers.
heuristic_window = 8
# If true, the agent must emit an explicit GROVE_EXIT (or equivalent) to finish successfully.
require_explicit_exit = true

[circuit_breaker]
# Open the breaker after this many iterations with no measurable progress.
no_progress_threshold = 3
# Open the breaker after this many repeated identical errors.
same_error_threshold = 5
# Open the breaker after this many permission-denied events.
permission_denial_threshold = 2
# After tripping, wait this many minutes before allowing a half-open retry.
cooldown_minutes = 30

[memory]
# Include playbook / lesson snippets in assembled prompts when available.
enable_playbook = true
# How many archive snippets to retrieve for context (top-k).
archive_top_k = 5
# Cap on snippet blocks injected from archive search.
max_prompt_snippets = 3
# Cap on bullet-style playbook items injected into the prompt.
max_prompt_bullets = 12

[reservations]
# Enable path reservation checks before dispatch (recommended).
enabled = true
# Default lease duration for a reservation (minutes) when not specified elsewhere.
default_ttl_minutes = 60

[safety]
# Scan session transcripts for unsafe patterns when enabled.
scan_transcripts = true
# Prepend a safety preamble to prompts when enabled.
inject_safety_preamble = true

[logging]
# Rust tracing log level for Grove (e.g. error, warn, info, debug, trace).
level = "info"
# Append structured JSON lines to the log file when true.
persist_jsonl = true
"#;
