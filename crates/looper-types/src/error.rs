use crate::loop_status::LoopStatus;
use crate::loop_target::LoopTargetType;
use crate::loop_type::LoopType;
use crate::run_status::RunStatus;

/// Errors raised by domain state machine and constraint validation.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum DomainError {
    #[error("invalid transition for loop status: {from} → {to}")]
    InvalidLoopStatusTransition { from: LoopStatus, to: LoopStatus },

    #[error("invalid transition for run status: {from} → {to}")]
    InvalidRunStatusTransition { from: RunStatus, to: RunStatus },

    #[error("step '{step}' does not belong to loop type {loop_type}")]
    StepNotInLoopType { step: String, loop_type: LoopType },

    #[error("loop type {loop_type} is not compatible with target type {target_type}")]
    LoopTypeTargetMismatch { loop_type: LoopType, target_type: LoopTargetType },

    #[error("'{value}' is not a valid loop status")]
    UnknownLoopStatus { value: String },

    #[error("'{value}' is not a valid run status")]
    UnknownRunStatus { value: String },

    #[error("'{value}' is not a valid loop type")]
    UnknownLoopType { value: String },

    #[error("'{value}' is not a valid failure kind")]
    UnknownFailureKind { value: String },

    #[error("'{value}' is not a valid resume policy")]
    UnknownResumePolicy { value: String },

    #[error("'{value}' is not a valid agent vendor")]
    UnknownAgentVendor { value: String },

    #[error("'{value}' is not a valid log level")]
    UnknownLogLevel { value: String },

    #[error("'{value}' is not a valid auth mode")]
    UnknownAuthMode { value: String },

    #[error("'{value}' is not a valid daemon mode")]
    UnknownDaemonMode { value: String },

    #[error("'{value}' is not a valid loop target type")]
    UnknownLoopTargetType { value: String },

    #[error("active loop conflict for type {loop_type} with target key '{target_key}' in project '{project_id}'")]
    ActiveLoopConflict { project_id: String, loop_type: LoopType, target_key: String },

    #[error("{0}")]
    Other(String),
}

impl From<String> for DomainError {
    fn from(msg: String) -> Self {
        DomainError::Other(msg)
    }
}

impl From<&str> for DomainError {
    fn from(msg: &str) -> Self {
        DomainError::Other(msg.to_string())
    }
}
