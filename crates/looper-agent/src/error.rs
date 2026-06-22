use std::process::ExitStatus;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum AgentError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("storage error: {0}")]
    Storage(#[from] looper_storage::error::StorageError),

    #[error("command not found: {0}")]
    CommandNotFound(String),

    #[error("invalid input: {0}")]
    InvalidInput(String),

    #[error("process timed out ({timeout_type})")]
    Timeout { timeout_type: String },

    #[error("process was killed: {reason}")]
    Killed { reason: String },

    #[error("parse error: {0}")]
    ParseError(String),

    #[error("native resume not supported for vendor {vendor}")]
    NativeResumeUnsupported { vendor: String },

    #[error("setup failure: {0}")]
    SetupFailure(String),

    #[error("process exited with status: {status}")]
    ProcessExit { status: ExitStatus },

    #[error("execution already completed")]
    AlreadyCompleted,

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, AgentError>;
