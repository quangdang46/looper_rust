use crate::{BeadId, SessionId, Timestamp};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventKind {
    BeadCacheSynced,
    DependencySnapshotSynced,
    GroveStatusUpdated,
    RunStarted,
    RunCheckpointed,
    RunSucceeded,
    RunFailed,
    SessionStarted,
    SessionCheckpointed,
    SessionSucceeded,
    SessionFailed,
    HandoffWritten,
    ReservationGranted,
    ReservationConflictDetected,
    ReservationExpired,
    RecoveryActionTaken,
    LeaseAcquired,
    LeaseHeartbeat,
    LeaseReleased,
    ShutdownRequested,
    SessionTerminationRequested,
    SessionTerminationForced,
    CoordinatorStopped,
    ArchiveIngested,
    PlaybookBulletAdded,
    PlaybookBulletPromoted,
    PlaybookBulletDeprecated,
    BrMirrorRequested,
    BrMirrorSucceeded,
    BrMirrorFailed,
    // New event kinds for observability
    ReactionInvoked,
    EscalationTierChanged,
    ActivityStateChanged,
    RecoveryCapsuleCreated,
}

/// Outcome classification for structured observability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventOutcome {
    /// Operation succeeded.
    Success,
    /// Operation failed with a classified error.
    Failure,
    /// Operation partially succeeded with warnings.
    Partial,
}

/// Structured error classification for observability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventError {
    /// Error class (e.g., "timeout", "permission_denied").
    pub class: String,
    /// Human-readable error message.
    pub message: String,
    /// Whether this error is retryable.
    pub retryable: bool,
}

/// Snapshot of context state at event time for debugging.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextSnapshot {
    /// Approximate context window usage percentage.
    pub context_usage_pct: Option<f32>,
    /// Number of active reservations.
    pub reservation_count: Option<i32>,
    /// Current escalation tier.
    pub escalation_tier: Option<String>,
    /// Current activity state.
    pub activity_state: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventLogRecord {
    pub id: i64,
    pub kind: EventKind,
    pub bead_id: Option<BeadId>,
    pub run_id: Option<crate::RunId>,
    pub session_id: Option<SessionId>,
    pub payload: Value,
    pub created_at: Timestamp,

    // New observability fields for post-mortem analysis
    /// Correlation ID to link related events across beads/runs.
    pub correlation_id: Option<String>,
    /// Operation name (e.g., "spawn", "check_context", "mirror").
    pub operation: Option<String>,
    /// Outcome of the event (success, failure, partial).
    pub outcome: Option<EventOutcome>,
    /// Duration of the operation in milliseconds.
    pub duration_ms: Option<u64>,
    /// Structured error details if the event failed.
    pub error: Option<EventError>,
    /// Context snapshot at event time for debugging.
    pub context_snapshot: Option<ContextSnapshot>,
}

impl EventLogRecord {
    /// Create a minimal event record (backwards compatible).
    #[must_use]
    pub fn minimal(id: i64, kind: EventKind, created_at: Timestamp) -> Self {
        Self {
            id,
            kind,
            bead_id: None,
            run_id: None,
            session_id: None,
            payload: Value::Null,
            created_at,
            correlation_id: None,
            operation: None,
            outcome: None,
            duration_ms: None,
            error: None,
            context_snapshot: None,
        }
    }

    /// Add correlation ID for linking related events.
    #[must_use]
    pub fn with_correlation(mut self, correlation_id: impl Into<String>) -> Self {
        self.correlation_id = Some(correlation_id.into());
        self
    }

    /// Add operation name for observability.
    #[must_use]
    pub fn with_operation(mut self, operation: impl Into<String>) -> Self {
        self.operation = Some(operation.into());
        self
    }

    /// Add outcome for observability.
    #[must_use]
    pub fn with_outcome(mut self, outcome: EventOutcome) -> Self {
        self.outcome = Some(outcome);
        self
    }

    /// Add duration for observability.
    #[must_use]
    pub fn with_duration(mut self, duration_ms: u64) -> Self {
        self.duration_ms = Some(duration_ms);
        self
    }

    /// Add error details for observability.
    #[must_use]
    pub fn with_error(
        mut self,
        class: impl Into<String>,
        message: impl Into<String>,
        retryable: bool,
    ) -> Self {
        self.error = Some(EventError {
            class: class.into(),
            message: message.into(),
            retryable,
        });
        self
    }

    /// Add context snapshot for debugging.
    #[must_use]
    pub fn with_context_snapshot(mut self, snapshot: ContextSnapshot) -> Self {
        self.context_snapshot = Some(snapshot);
        self
    }
}

/// Aggregated metrics for a run (for `grove inspect` command).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunMetrics {
    pub run_id: crate::RunId,
    pub total_duration_secs: u64,
    pub checkpoints_taken: u32,
    pub retries_attempted: u32,
    pub rescue_injections: u32,
    pub reactions_invoked: u32,
    pub max_escalation_tier: u32,
    pub termination_reason: Option<String>,
}

/// Run report for post-mortem analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunReport {
    pub run_id: crate::RunId,
    pub bead_id: BeadId,
    pub status: crate::RunStatus,
    pub metrics: RunMetrics,
    pub failure_class: Option<crate::FailureClass>,
    pub recovery_capsule: Option<crate::RecoveryCapsule>,
    pub event_count: u32,
    pub first_event_at: Option<Timestamp>,
    pub last_event_at: Option<Timestamp>,
}
