use thiserror::Error;

/// Errors that can occur during webhook processing and forwarding.
#[derive(Debug, Error)]
pub enum WebhookError {
    #[error("webhook event ignored: {0}")]
    Ignored(String),

    #[error("unknown event type: {0}")]
    UnknownEventType(String),

    #[error("payload parse error: {0}")]
    ParseError(#[from] serde_json::Error),

    #[error("storage error: {0}")]
    Storage(#[from] looper_storage::error::StorageError),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("timeout")]
    Timeout,

    #[error("rate limited")]
    RateLimited,

    #[error("forwarder closed")]
    Closed,

    #[error("no project configured for repo: {0}")]
    NoProject(String),

    #[error("project not found: {0}")]
    ProjectNotFound(String),

    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    #[error("internal error: {0}")]
    Internal(String),
}

impl WebhookError {
    /// Returns true if the error is transient and the operation can be retried.
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            WebhookError::Timeout | WebhookError::RateLimited | WebhookError::Io(_) | WebhookError::Storage(_)
        )
    }
}

/// Check if an error message suggests a transient condition.
pub fn is_transient_error(err: &WebhookError) -> bool {
    if err.is_transient() {
        return true;
    }
    let msg = err.to_string().to_lowercase();
    msg.contains("timeout") || msg.contains("tempor") || msg.contains("rate limit") || msg.contains("retry after")
}
