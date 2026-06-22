use thiserror::Error;

/// Errors raised by the runner layer.
#[derive(Debug, Error)]
pub enum RunnerError {
    // ── Downstream ──
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("storage error: {0}")]
    Storage(#[from] looper_storage::StorageError),

    #[error("service error: {0}")]
    Service(#[from] looper_service::ServiceError),

    #[error("GitHub error: {0}")]
    GitHub(#[from] looper_github::error::GitHubError),

    #[error("Git error: {0}")]
    Git(#[from] looper_git::error::GitError),

    #[error("agent error: {0}")]
    Agent(#[from] looper_agent::error::AgentError),

    #[error("scheduler error: {0}")]
    Scheduler(#[from] looper_scheduler::SchedulerError),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("chrono error: {0}")]
    Chrono(#[from] chrono::ParseError),

    #[error("regex error: {0}")]
    Regex(#[from] regex::Error),

    // ── Runner business logic ──
    #[error("step '{step}' failed: {message}")]
    StepFailure { step: String, message: String },

    #[error("missing prerequisite for step '{step}': {message}")]
    MissingPrerequisite { step: String, message: String },

    #[error("manual intervention required: {0}")]
    ManualIntervention(String),

    #[error("non-retryable error: {0}")]
    NonRetryable(String),

    #[error("invalid state transition: {0}")]
    InvalidTransition(String),

    #[error("checkpoint error: {0}")]
    Checkpoint(String),

    #[error("queue item {0} not found")]
    QueueItemNotFound(String),

    #[error("run {0} not found")]
    RunNotFound(String),

    #[error("handler not configured: {0}")]
    HandlerNotConfigured(String),

    #[error("{0}")]
    Other(String),
}

impl From<String> for RunnerError {
    fn from(msg: String) -> Self {
        RunnerError::Other(msg)
    }
}

impl From<&str> for RunnerError {
    fn from(msg: &str) -> Self {
        RunnerError::Other(msg.to_string())
    }
}

pub type RunnerResult<T> = std::result::Result<T, RunnerError>;
