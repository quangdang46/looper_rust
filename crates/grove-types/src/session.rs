use crate::{PromptId, ProtocolEvent, SessionId, Timestamp, errors::InvalidTransition};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionStatus {
    Starting,
    Running,
    Checkpointed,
    Completed,
    TimedOut,
    RateLimited,
    PermissionDenied,
    Crashed,
    UnknownFailure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StopReason {
    Exit,
    Checkpoint,
    Timeout,
    RateLimit,
    PermissionDenied,
    Crash,
    Kill,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CircuitState {
    Closed,
    HalfOpen,
    Open,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ProgressSignal {
    #[default]
    None,
    Weak,
    Moderate,
    Strong,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionTerminalClass {
    Success,
    Checkpoint,
    Timeout,
    RateLimit,
    PermissionDenied,
    Crash,
    VerifyFailed,
    UnknownFailure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ContextPressureLevel {
    #[default]
    Ok,
    Warn,
    Rotate,
    HardStop,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IterationAnalysis {
    pub output_lines: usize,
    pub output_chars: usize,
    pub completion_indicators: u32,
    pub has_explicit_exit_true: bool,
    pub has_explicit_exit_false: bool,
    pub checkpoint_emitted: bool,
    pub probable_progress: ProgressSignal,
    pub permission_denials: u32,
    pub rate_limit_markers: u32,
    pub repeated_error_fingerprint: Option<String>,
    pub artifacts_mentioned: Vec<String>,
    pub lessons: Vec<String>,
    pub decisions: Vec<String>,
    pub warnings: Vec<String>,
    pub estimated_prompt_tokens: u32,
    pub estimated_output_tokens: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeSessionRecord {
    pub id: SessionId,
    pub run_id: crate::RunId,
    pub external_session_id: Option<String>,
    pub ordinal_in_run: i32,
    pub status: SessionStatus,
    pub started_at: Timestamp,
    pub ended_at: Option<Timestamp>,
    pub prompt_id: Option<PromptId>,
    pub prompt_manifest_path: Option<String>,
    pub prompt_bytes: i32,
    pub estimated_input_tokens: i32,
    pub estimated_output_tokens: i32,
    pub exit_code: Option<i32>,
    pub stop_reason: Option<StopReason>,
    pub transcript_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionOutcome {
    pub session: ClaudeSessionRecord,
    pub protocol_events: Vec<ProtocolEvent>,
    pub analysis: IterationAnalysis,
    pub terminal_class: SessionTerminalClass,
    pub context_pressure_pct: Option<f32>,
    pub context_pressure_level: ContextPressureLevel,
    pub stdout_tail: Vec<String>,
    pub stderr_tail: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CircuitBreakerState {
    pub state: CircuitState,
    pub no_progress_count: u32,
    pub same_error_count: u32,
    pub permission_denial_count: u32,
    pub last_error_fingerprint: Option<String>,
    pub opened_at: Option<Timestamp>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TranscriptEvent {
    SessionStarted {
        session_id: SessionId,
        ts: Timestamp,
    },
    StdoutLine {
        line: String,
        ts: Timestamp,
    },
    StderrLine {
        line: String,
        ts: Timestamp,
    },
    ParsedProtocol {
        event: ProtocolEvent,
        ts: Timestamp,
    },
    SessionEnded {
        exit_code: Option<i32>,
        ts: Timestamp,
    },
}

impl SessionStatus {
    #[must_use]
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Starting | Self::Running)
    }
}

impl ClaudeSessionRecord {
    #[must_use]
    pub fn can_transition_to(&self, next: SessionStatus) -> bool {
        use SessionStatus::{
            Checkpointed, Completed, Crashed, PermissionDenied, RateLimited, Running, Starting,
            TimedOut, UnknownFailure,
        };

        matches!(
            (self.status, next),
            (Starting, Running)
                | (Running, Completed)
                | (Running, Checkpointed)
                | (Running, TimedOut)
                | (Running, RateLimited)
                | (Running, PermissionDenied)
                | (Running, Crashed)
                | (Running, UnknownFailure)
        )
    }

    pub fn ensure_transition(self, next: SessionStatus) -> Result<Self, InvalidTransition> {
        if self.can_transition_to(next) {
            Ok(Self {
                status: next,
                ..self
            })
        } else {
            Err(InvalidTransition::new(
                "session",
                format!("{:?}", self.status),
                format!("{:?}", next),
            ))
        }
    }
}

impl CircuitState {
    #[must_use]
    pub fn can_transition_to(self, next: Self) -> bool {
        use CircuitState::{Closed, HalfOpen, Open};

        matches!(
            (self, next),
            (Closed, HalfOpen)
                | (Closed, Open)
                | (HalfOpen, Closed)
                | (HalfOpen, Open)
                | (Open, HalfOpen)
        )
    }
}

impl Default for CircuitBreakerState {
    fn default() -> Self {
        Self {
            state: CircuitState::Closed,
            no_progress_count: 0,
            same_error_count: 0,
            permission_denial_count: 0,
            last_error_fingerprint: None,
            opened_at: None,
        }
    }
}
