use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

/// CLI tool vendors — distinct from looper-types AgentVendor (which is model provider).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentCliVendor {
    #[serde(rename = "claude-code")]
    ClaudeCode,
    Codex,
    Opencode,
    #[serde(rename = "cursor-cli")]
    CursorCli,
    Hermes,
}

impl AgentCliVendor {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
            Self::Opencode => "opencode",
            Self::CursorCli => "cursor-cli",
            Self::Hermes => "hermes",
        }
    }

    /// Whether this vendor supports native resume.
    pub fn native_resume_supported(&self) -> bool {
        !matches!(self, Self::Hermes)
    }

    /// Default binary name for this vendor.
    pub fn default_binary(&self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude",
            Self::Codex => "codex",
            Self::Opencode => "opencode",
            Self::CursorCli => "agent",
            Self::Hermes => "hermes",
        }
    }

    /// Model flag for this vendor (e.g. "--model" or "-m").
    pub fn model_flag(&self) -> &'static str {
        match self {
            Self::Hermes => "-m",
            _ => "--model",
        }
    }

    /// Prompt flag for this vendor (e.g. "--print" or "-z").
    pub fn prompt_flag(&self) -> &'static str {
        match self {
            Self::Codex => "", // prompt is positional after `exec`
            Self::Hermes => "-z",
            _ => "--print",
        }
    }

    /// Subcommand to force (e.g. "exec" for codex, "run" for opencode).
    pub fn forced_subcommand(&self) -> Option<&'static str> {
        match self {
            Self::Codex => Some("exec"),
            Self::Opencode => Some("run"),
            _ => None,
        }
    }

    /// Resume flag (e.g. "--resume", "--session", or the positional resume for codex).
    pub fn resume_flag(&self) -> &'static str {
        match self {
            Self::Codex => "", // handled as positional subcommand `resume <sessionID>`
            Self::Opencode => "--session",
            _ => "--resume",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutorConfig {
    pub vendor: AgentCliVendor,
    pub model: Option<String>,
    #[serde(default)]
    pub params: HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default = "default_native_resume_enabled")]
    pub native_resume_enabled: bool,
}

fn default_native_resume_enabled() -> bool {
    true
}

#[derive(Debug, Clone)]
pub struct RunInput {
    pub execution_id: String,
    pub project_id: String,
    pub loop_id: String,
    pub run_id: String,
    pub prompt: String,
    pub native_resume_prompt: Option<String>,
    pub working_directory: String,
    pub timeout: Duration,
    pub heartbeat_timeout: Duration,
    pub graceful_shutdown: Duration,
    pub max_output_bytes: usize,
    pub metadata: HashMap<String, serde_json::Value>,
    pub idempotency_key: String,
    pub env: HashMap<String, String>,
    pub native_session_id: Option<String>,
}

impl Default for RunInput {
    fn default() -> Self {
        Self {
            execution_id: String::new(),
            project_id: String::new(),
            loop_id: String::new(),
            run_id: String::new(),
            prompt: String::new(),
            native_resume_prompt: None,
            working_directory: String::new(),
            timeout: Duration::from_secs(1800),
            heartbeat_timeout: Duration::from_secs(300),
            graceful_shutdown: Duration::from_secs(5),
            max_output_bytes: 256 * 1024,
            metadata: HashMap::new(),
            idempotency_key: String::new(),
            env: HashMap::new(),
            native_session_id: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentResult {
    pub status: String,
    pub summary: String,
    pub stdout: String,
    pub stderr: String,
    pub parse_status: String,
    pub completion_signal: Option<String>,
    pub artifacts: Vec<String>,
    pub changed_files: Vec<String>,
    pub commits: Vec<String>,
    pub heartbeat_count: i64,
    pub timeout_type: Option<String>,
    pub configured_idle_timeout_seconds: i64,
    pub configured_max_runtime_seconds: i64,
    pub elapsed_runtime_seconds: i64,
    pub last_progress_at: Option<String>,
    pub pid: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NativeResumeMode {
    #[serde(rename = "native_resume")]
    NativeResume,
    #[serde(rename = "checkpoint_restart")]
    CheckpointRestart,
}

impl NativeResumeMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NativeResume => "native_resume",
            Self::CheckpointRestart => "checkpoint_restart",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NativeResumeStatus {
    Started,
    Disabled,
    Unsupported,
    Unavailable,
    Pending,
    Captured,
    Failed,
    #[serde(rename = "fallback_started")]
    FallbackStarted,
    #[serde(rename = "fallback_completed")]
    FallbackCompleted,
    #[serde(rename = "fallback_failed")]
    FallbackFailed,
}

impl NativeResumeStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Disabled => "disabled",
            Self::Unsupported => "unsupported",
            Self::Unavailable => "unavailable",
            Self::Pending => "pending",
            Self::Captured => "captured",
            Self::Failed => "failed",
            Self::FallbackStarted => "fallback_started",
            Self::FallbackCompleted => "fallback_completed",
            Self::FallbackFailed => "fallback_failed",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExecutionState {
    pub pid: Option<u32>,
    pub start_time: chrono::DateTime<chrono::Utc>,
    pub last_output_time: chrono::DateTime<chrono::Utc>,
    pub stdout_buffer: Vec<u8>,
    pub stderr_buffer: Vec<u8>,
    pub timed_out: bool,
    pub killed: bool,
    pub completed: bool,
    pub timeout_type: Option<String>,
    pub heartbeat_count: i64,
    pub native_session_id: Option<String>,
}

pub struct SpawnCommand {
    pub binary: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeoutType {
    MaxRuntime,
    Idle,
}

impl TimeoutType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::MaxRuntime => "max_runtime",
            Self::Idle => "idle",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionParseStatus {
    Parsed,
    Missing,
    InvalidJson,
}

impl CompletionParseStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Parsed => "parsed",
            Self::Missing => "missing",
            Self::InvalidJson => "invalid_json",
        }
    }
}

impl From<&str> for CompletionParseStatus {
    fn from(s: &str) -> Self {
        match s {
            "parsed" => Self::Parsed,
            "invalid_json" => Self::InvalidJson,
            _ => Self::Missing,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionPayload {
    pub summary: String,
    #[serde(default)]
    pub artifacts: Vec<String>,
    #[serde(default)]
    pub changed_files: Vec<String>,
    #[serde(default)]
    pub commits: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_pr_lifecycle: Option<serde_json::Value>,
}

pub const COMPLETION_MARKER: &str = "__LOOPER_RESULT__=";
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 256 * 1024;
