mod analysis;
mod analyzer;
mod backend;
mod exit_policy;
mod materializer;
mod parser;
mod progress;
mod protocol;
mod retry;
mod runner;
mod transcript;
mod verify;

pub use analysis::{AnalysisInput, analyze_iteration};
pub use analyzer::{
    ContextMonitor, ContextPressure, ContextPressureDecision, SessionAnalysisContext,
    analyze_session_outcome, classify_session_outcome, classify_session_outcome_with_policy,
    evaluate_exit_policy, evaluate_outcome_exit_policy, update_circuit_breaker,
};
pub use backend::{ClaudeBackend, CliClaudeBackend, RunningSession, StartSessionRequest};
pub use exit_policy::{ExitDecision, ExitPolicy};
pub use materializer::{
    CheckpointPromptInput, PromptMaterialization, PromptMaterializationInput, materialize_prompt,
};
pub use parser::{ParserLineKind, ProtocolParser, ProtocolWarning};
pub use progress::infer_progress_signal;
pub use protocol::{
    GROVE_ARTIFACTS_PREFIX, GROVE_CHECKPOINT_PREFIX, GROVE_DECISIONS_PREFIX, GROVE_EXIT_PREFIX,
    GROVE_LESSONS_PREFIX, GROVE_RESULT_PREFIX, GROVE_WARNINGS_PREFIX, ProtocolMarker,
    ProtocolParseError, parse_protocol_event,
};
pub use retry::{RetryMutationPlan, plan_retry_mutation};
pub use runner::{
    SessionLifecycleHooks, SessionShutdownConfig, SingleTaskSessionRequest,
    SingleTaskSessionResult, SingleTaskSessionRunnerError, execute_single_task_session,
    execute_single_task_session_with_hooks,
};
pub use transcript::{TranscriptError, TranscriptReplay, TranscriptWriter, replay_transcript};
pub use verify::{VerificationMode, run_verification};

pub const CRATE_PURPOSE: &str =
    "Claude session protocol parsing, transcript capture, and session analysis helpers.";
