use thiserror::Error;

/// Errors raised by the scheduler and its subsystems.
#[derive(Debug, Error)]
pub enum SchedulerError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("storage error: {0}")]
    Storage(#[from] looper_storage::StorageError),

    #[error("service error: {0}")]
    Service(#[from] looper_service::ServiceError),

    #[error("scheduler is already running")]
    AlreadyRunning,

    #[error("scheduler is shutting down")]
    ShuttingDown,

    #[error("handler not configured: {0}")]
    HandlerNotConfigured(String),

    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    #[error("claim lock contention")]
    ClaimLockContention,

    #[error("queue item {0} not found")]
    QueueItemNotFound(String),

    #[error("unable to resolve processor for queue item type '{0}'")]
    UnresolvableProcessor(String),

    #[error("recovery interrupted")]
    RecoveryInterrupted,

    #[error("cancelled")]
    Cancelled,

    #[error("{0}")]
    Other(String),
}

pub type SchedulerResult<T> = std::result::Result<T, SchedulerError>;
