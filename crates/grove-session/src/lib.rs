mod analysis;
mod analyzer;
mod backend;
mod exit_policy;
mod parser;
mod progress;
mod protocol;
mod transcript;

pub use analysis::{analyze_iteration, AnalysisInput};
pub use analyzer::{
    analyze_session_outcome, evaluate_exit_policy, evaluate_outcome_exit_policy,
    SessionAnalysisContext,
};
pub use backend::{ClaudeBackend, CliClaudeBackend, RunningSession, StartSessionRequest};
pub use exit_policy::{ExitDecision, ExitPolicy};
pub use parser::{ParserLineKind, ProtocolParser, ProtocolWarning};
pub use progress::infer_progress_signal;
pub use protocol::{
    parse_protocol_event, ProtocolMarker, ProtocolParseError, GROVE_ARTIFACTS_PREFIX,
    GROVE_CHECKPOINT_PREFIX, GROVE_DECISIONS_PREFIX, GROVE_EXIT_PREFIX, GROVE_LESSONS_PREFIX,
    GROVE_RESULT_PREFIX, GROVE_WARNINGS_PREFIX,
};
pub use transcript::{replay_transcript, TranscriptError, TranscriptReplay, TranscriptWriter};

pub const CRATE_PURPOSE: &str =
    "Claude session protocol parsing, transcript capture, and session analysis helpers.";
