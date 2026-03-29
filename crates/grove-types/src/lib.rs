pub mod archive;
mod checkpoint;
mod errors;
mod event;
mod handoff;
mod ids;
mod ops;
pub mod playbook;
mod priority;
pub mod prompt;
pub mod reaction;
mod reservation;
mod run;
mod session;
mod task;
mod time;
mod workflow;

pub use archive::{
    ConversationRecord, MessageRecord, MessageRole, RelevantSnippet, RetrievalBundle,
    SnippetRecord, SourceRecord,
};
pub use checkpoint::{
    CheckpointPayload, CheckpointRecord, ProtocolEvent, ProtocolState, ResumeGeneration,
};
pub use errors::{GroveTypesError, InvalidTransition};
pub use event::{
    ContextSnapshot, EventError, EventKind, EventLogRecord, EventOutcome, RunMetrics, RunReport,
};
pub use handoff::HandoffRecord;
pub use ids::{BeadId, BulletId, CheckpointId, PromptId, RunId, SessionId, SourceId, TickId};
pub use ops::{
    CleanupSnapshotRecord, ConfigSnapshotRecord, DispatchDecisionRecord, IntegrityCheckRecord,
    PromptMaterializationRecord,
};
pub use playbook::{
    BulletMaturity, BulletScope, BulletState, BulletType, FeedbackEventRecord, FeedbackKind,
    MemoryDiaryRecord, PlaybookBulletRecord,
};
pub use priority::BeadPriority;
pub use prompt::{
    EscalationContext, ExecutionContract, PromptManifest, PromptManifestSection,
    PromptSectionProvenance, PromptSegment, PromptSegmentKind, PromptTrimReason,
};
pub use reaction::{
    MutationStrategy, ReactionAction, ReactionContextSnapshot, ReactionOutcome, ReactionRecord,
    ReactionRule, ReactionTrigger, default_reactions,
};
pub use reservation::{ReservationConflict, ReservationMode, ReservationRecord};
pub use run::{
    AgentActivity, AutonomousAction, CoordinatorStopReason, EscalationPolicy, EscalationTier,
    FailureClass, LeaderLeaseRecord, MirrorOutboxRecord, MirrorStatus, RecoveryCapsule,
    RecoveryCapsuleOutcome, RetryPolicy, RunStatus, TaskRunRecord,
};
pub use session::{
    CircuitBreakerState, CircuitState, ClaudeSessionRecord, ContextPressureLevel,
    IterationAnalysis, ProgressSignal, RuntimeProvider, SessionOutcome, SessionStatus,
    SessionTerminalClass, StopReason, TranscriptEvent,
};
pub use task::{BeadRef, BeadRuntimePatch, GroveBeadRecord, GroveBeadStatus};
pub use time::Timestamp;
pub use workflow::{WorkflowPhase, WorkflowState};

pub const CRATE_PURPOSE: &str = "Shared Grove domain types and identifiers.";

#[cfg(test)]
mod tests;
