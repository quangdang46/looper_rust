//! GitHub gateway error types.

use std::fmt;

/// A transient error that should be retried.
#[derive(Debug)]
pub struct TransientError {
    pub inner: Box<dyn std::error::Error + Send + Sync>,
}

impl TransientError {
    pub fn new<E: Into<Box<dyn std::error::Error + Send + Sync>>>(inner: E) -> Self {
        Self {
            inner: inner.into(),
        }
    }
}

impl fmt::Display for TransientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "transient GitHub error: {}", self.inner)
    }
}

impl std::error::Error for TransientError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&*self.inner)
    }
}

/// Errors originating from the GitHub gateway.
#[derive(Debug, thiserror::Error)]
pub enum GitHubError {
    /// The `gh` CLI was not found or failed to execute.
    #[error("gh CLI execution error: {0}")]
    CommandExecution(String),
    /// Non-transient gh CLI failure with stderr.
    #[error("gh command failed: {0}")]
    CommandFailed(String),
    /// JSON parsing failure.
    #[error("JSON parse error: {0}")]
    JsonParse(#[from] serde_json::Error),
    /// A transient error wrapping the inner cause.
    #[error(transparent)]
    Transient(#[from] TransientError),
    /// HTTP-level error from the GitHub API.
    #[error("GitHub API error: {0} (status: {1})")]
    Api(String, u16),
    /// Authentication failure.
    #[error("GitHub auth error: {0}")]
    Auth(String),
    /// Resource not found.
    #[error("not found: {0}")]
    NotFound(String),
    /// The requested repo/resource has no data.
    #[error("empty: {0}")]
    Empty(String),
    /// Diff too large to process.
    #[error("diff too large: {0}")]
    DiffTooLarge(String),
    /// Review thread not found.
    #[error("review thread not found: {thread_id}")]
    ReviewThreadNotFound { thread_id: String },
    /// IO error from subprocess execution.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// Request error from reqwest.
    #[error("HTTP request error: {0}")]
    Reqwest(#[from] reqwest::Error),
    /// Rate limit exceeded.
    #[error("rate limit exceeded: {0}")]
    RateLimit(String),
    /// Invalid input.
    #[error("invalid input: {0}")]
    InvalidInput(String),
    /// Catch-all for other errors.
    #[error("{0}")]
    Other(String),
}

// ---------------------------------------------------------------------------
// Error classification helpers
// ---------------------------------------------------------------------------

/// Transient error messages that trigger automatic retry.
pub(crate) const TRANSIENT_PATTERNS: &[&str] = &[
    "tls handshake timeout",
    "unexpected eof",
    "connection reset by peer",
    "connection refused",
    "connection timed out",
    "i/o timeout",
    "temporary failure in name resolution",
    "no such host",
    "network is unreachable",
    "stream error",
    "http2: server sent goaway",
    "http 502",
    "502 bad gateway",
    "http 503",
    "503 service unavailable",
    "http 504",
    "504 gateway timeout",
    "secondary rate limit",
    "rate limit exceeded",
    "api rate limit exceeded",
    "graphql: something went wrong",
];

/// Returns `true` if the error is transient (should be retried).
pub fn is_transient_error(err: &GitHubError) -> bool {
    match err {
        GitHubError::Transient(_) => true,
        GitHubError::CommandExecution(msg) | GitHubError::CommandFailed(msg) => {
            let lower = msg.to_lowercase();
            TRANSIENT_PATTERNS
                .iter()
                .any(|p| lower.contains(p))
        }
        GitHubError::RateLimit(_) => true,
        _ => false,
    }
}

/// Extracts the most useful user-facing text from a GitHubError.
pub fn error_message(err: &GitHubError) -> String {
    match err {
        GitHubError::CommandExecution(msg) | GitHubError::CommandFailed(msg) => msg.clone(),
        GitHubError::JsonParse(e) => e.to_string(),
        GitHubError::Transient(t) => t.inner.to_string(),
        GitHubError::Api(msg, code) => format!("{} (HTTP {})", msg, code),
        GitHubError::Auth(msg) => msg.clone(),
        GitHubError::NotFound(msg) => msg.clone(),
        GitHubError::Empty(msg) => msg.clone(),
        GitHubError::DiffTooLarge(msg) => msg.clone(),
        GitHubError::ReviewThreadNotFound { thread_id } => thread_id.clone(),
        GitHubError::Io(e) => e.to_string(),
        GitHubError::Reqwest(e) => e.to_string(),
        GitHubError::RateLimit(msg) => msg.clone(),
        GitHubError::InvalidInput(msg) => msg.clone(),
        GitHubError::Other(msg) => msg.clone(),
    }
}

/// Returns `true` if the error indicates a pull request was not found.
pub fn is_pull_request_not_found_error(err: &GitHubError) -> bool {
    match err {
        GitHubError::NotFound(msg) | GitHubError::CommandFailed(msg) => {
            msg.contains("could not resolve to a pullrequest")
                || msg.contains("pull request not found")
                || msg.contains("no pull request matches")
        }
        _ => false,
    }
}

/// Returns `true` if the error is an HTTP 404 (not found).
pub fn is_not_found_error(err: &GitHubError) -> bool {
    match err {
        GitHubError::NotFound(_) => true,
        GitHubError::CommandFailed(msg) => msg.contains("http 404") || msg.contains(" 404"),
        GitHubError::Api(_, code) => *code == 404,
        _ => false,
    }
}

/// Returns `true` if the error is an inaccessible review request reviewer error.
pub fn is_inaccessible_review_request_reviewer_error(err: &GitHubError) -> bool {
    match err {
        GitHubError::CommandFailed(msg) => {
            let lower = msg.to_lowercase();
            lower.contains("resource not accessible")
                && lower.contains("reviewrequests")
                && lower.contains("requestedreviewer")
        }
        _ => false,
    }
}

/// Returns `true` if the error indicates the diff is too large.
pub fn is_diff_too_large_error(err: &GitHubError) -> bool {
    matches!(err, GitHubError::DiffTooLarge(_))
}
