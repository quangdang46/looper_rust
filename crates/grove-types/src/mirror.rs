use crate::{BeadId, RunId, Timestamp};
use serde::{Deserialize, Serialize};

/// Mirror operation types that can be retried
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MirrorOperationType {
    /// Close a bead with optional reason
    Close,
    /// Update bead status
    UpdateStatus,
    /// Add comment to bead
    AddComment,
    /// Flush/sync bead cache
    Sync,
}

/// Current state of a mirror operation in the outbox
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MirrorState {
    /// Operation is pending retry
    Pending,
    /// Operation is currently being attempted
    InFlight,
    /// Operation succeeded and was mirrored
    Succeeded,
    /// Operation failed permanently (after max retries)
    Failed,
}

/// Single mirror operation record in the outbox
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MirrorOperation {
    pub id: i64,
    pub bead_id: BeadId,
    pub run_id: RunId,
    pub operation_type: MirrorOperationType,
    pub state: MirrorState,
    pub payload_json: String,
    pub attempt_count: i32,
    pub last_attempt_at: Option<Timestamp>,
    pub last_error: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// Summary of mirror status for a bead
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BeadMirrorStatus {
    pub bead_id: BeadId,
    pub local_status: String,
    pub mirror_state: MirrorState,
    pub pending_operations: i32,
    pub failed_operations: i32,
    pub last_mirror_attempt: Option<Timestamp>,
    pub last_error: Option<String>,
}

/// Result of a mirror batch attempt
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MirrorBatchResult {
    pub succeeded: i32,
    pub failed: i32,
    pub still_pending: i32,
    pub errors: Vec<MirrorError>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorError {
    pub bead_id: BeadId,
    pub operation_id: i64,
    pub operation_type: MirrorOperationType,
    pub error_message: String,
}

/// Payload for closing a bead
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClosePayload {
    pub reason: Option<String>,
    pub comment: Option<String>,
}

/// Payload for updating bead status
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateStatusPayload {
    pub status: String,
}

/// Payload for adding a comment
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddCommentPayload {
    pub text: String,
}

impl MirrorOperation {
    #[must_use]
    pub fn new(
        id: i64,
        bead_id: BeadId,
        run_id: RunId,
        operation_type: MirrorOperationType,
        payload_json: String,
        created_at: Timestamp,
    ) -> Self {
        Self {
            id,
            bead_id,
            run_id,
            operation_type,
            state: MirrorState::Pending,
            payload_json,
            attempt_count: 0,
            last_attempt_at: None,
            last_error: None,
            created_at,
            updated_at: created_at,
        }
    }

    #[must_use]
    pub fn can_retry(&self, max_attempts: i32) -> bool {
        self.state == MirrorState::Pending && self.attempt_count < max_attempts
    }

    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self.state, MirrorState::Succeeded | MirrorState::Failed)
    }

    fn record_attempt(&mut self, timestamp: Timestamp) {
        self.attempt_count += 1;
        self.last_attempt_at = Some(timestamp);
        self.updated_at = timestamp;
    }

    pub fn mark_in_flight(&mut self, timestamp: Timestamp) {
        self.state = MirrorState::InFlight;
        self.record_attempt(timestamp);
    }

    pub fn mark_success(&mut self, timestamp: Timestamp) {
        self.state = MirrorState::Succeeded;
        self.record_attempt(timestamp);
        self.last_error = None;
    }

    pub fn mark_failure(&mut self, error: String, timestamp: Timestamp, max_attempts: i32) {
        self.record_attempt(timestamp);
        self.last_error = Some(error);

        if self.attempt_count >= max_attempts {
            self.state = MirrorState::Failed;
        } else {
            self.state = MirrorState::Pending;
        }
    }
}

impl MirrorState {
    #[must_use]
    pub const fn is_pending(&self) -> bool {
        matches!(self, Self::Pending)
    }

    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed)
    }

    #[must_use]
    pub const fn display_name(&self) -> &str {
        match self {
            Self::Pending => "pending",
            Self::InFlight => "in-flight",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }
}

impl MirrorBatchResult {
    pub fn record_success(&mut self) {
        self.succeeded += 1;
    }

    pub fn record_failure(&mut self, error: MirrorError) {
        self.failed += 1;
        self.errors.push(error);
    }

    pub fn record_pending(&mut self) {
        self.still_pending += 1;
    }

    #[must_use]
    pub const fn total_attempted(&self) -> i32 {
        self.succeeded + self.failed
    }

    #[must_use]
    pub const fn has_failures(&self) -> bool {
        self.failed > 0 || !self.errors.is_empty()
    }

    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.still_pending == 0
    }
}

#[cfg(test)]
mod tests;
