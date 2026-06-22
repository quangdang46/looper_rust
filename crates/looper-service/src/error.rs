use thiserror::Error;

use looper_storage::StorageError;
use looper_types::DomainError;

/// Errors raised by the service layer.
#[derive(Error, Debug)]
pub enum ServiceError {
    // ── Not found ──
    #[error("loop not found: {0}")]
    LoopNotFound(String),

    #[error("run not found: {0}")]
    RunNotFound(String),

    #[error("project not found: {0}")]
    ProjectNotFound(String),

    // ── Business logic ──
    #[error("loop '{loop_id}' already has a running run")]
    LoopHasRunningRun { loop_id: String },

    #[error("active loop conflict for type {loop_type}/{target_key} in project {project_id}")]
    ActiveLoopConflict {
        project_id: String,
        loop_type: String,
        target_key: String,
    },

    #[error("invalid project ID: {0}")]
    InvalidProjectID(String),

    #[error("project ID collision: {0}")]
    ProjectIDCollision(String),

    #[error("ambiguous project identifier: {0}")]
    AmbiguousProjectIdentifier(String),

    #[error("project is managed by config and cannot be removed: {0}")]
    ConfigManagedProject(String),

    #[error("reviewer auto-merge validation failed: {0}")]
    ReviewerAutoMergeValidation(String),

    // ── Wrapped downstream errors ──
    #[error("domain error: {0}")]
    Domain(#[from] DomainError),

    #[error("storage error: {0}")]
    Storage(#[from] StorageError),

    #[error("event log error: {0}")]
    EventLog(String),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, ServiceError>;

impl From<String> for ServiceError {
    fn from(msg: String) -> Self {
        ServiceError::Other(msg)
    }
}

impl From<&str> for ServiceError {
    fn from(msg: &str) -> Self {
        ServiceError::Other(msg.to_string())
    }
}
