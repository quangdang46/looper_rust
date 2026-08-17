//! Human-in-the-Loop (HITL) — decision points for complex scenarios.
//!
//! When the daemon encounters a situation it can't resolve autonomously
//! (merge conflict, failing CI, ambiguous feedback), it pauses the loop
//! and asks a human for direction via a configured transport.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Decision a human can make about a stuck loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HitlDecision {
    /// Resume the loop from the current state.
    Retry,
    /// Skip the current step and move on.
    Skip,
    /// Attempt to merge the PR as-is.
    Merge,
    /// Abort the loop entirely.
    Abort,
    /// Custom instruction from the human.
    Custom(String),
}

impl fmt::Display for HitlDecision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Retry => write!(f, "retry"),
            Self::Skip => write!(f, "skip"),
            Self::Merge => write!(f, "merge"),
            Self::Abort => write!(f, "abort"),
            Self::Custom(s) => write!(f, "custom: {s}"),
        }
    }
}

/// Why HITL was triggered.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HitlReason {
    /// Merge conflict after multiple fix attempts.
    MergeConflict,
    /// Failing CI after N retry attempts.
    FailingCi,
    /// Ambiguous reviewer feedback.
    AmbiguousFeedback,
    /// Manual intervention needed.
    ManualIntervention,
    /// Policy violation.
    PolicyViolation,
}

impl fmt::Display for HitlReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MergeConflict => write!(f, "merge_conflict"),
            Self::FailingCi => write!(f, "failing_ci"),
            Self::AmbiguousFeedback => write!(f, "ambiguous_feedback"),
            Self::ManualIntervention => write!(f, "manual_intervention"),
            Self::PolicyViolation => write!(f, "policy_violation"),
        }
    }
}

/// A HITL request sent to a human via a transport.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HitlRequest {
    /// Unique request ID.
    pub id: String,
    /// Run ID that triggered this request.
    pub run_id: String,
    /// Loop ID that triggered this request.
    pub loop_id: String,
    /// Project name.
    pub project_name: String,
    /// Why HITL was triggered.
    pub reason: HitlReason,
    /// Context (PR link, error details, etc.).
    pub context: serde_json::Value,
    /// When the request was created.
    pub created_at: String,
    /// When the request expires (optional).
    pub expires_at: Option<String>,
}

/// Transport interface for delivering HITL requests to humans.
///
/// Implementations:
/// - `GitHubCommentTransport`: PR comments with `/looper` slash commands
/// - `DashboardTransport`: SSE events → UI → POST response
/// - `SlackTransport`: (stretch goal) Slack messages with buttons
#[async_trait]
pub trait HitlTransport: Send + Sync {
    /// Deliver a HITL request and wait for a human response.
    async fn request_decision(&self, request: &HitlRequest) -> Result<HitlDecision, HitlError>;

    /// Check if a response has arrived for a pending request.
    async fn check_response(&self, request_id: &str) -> Result<Option<HitlDecision>, HitlError>;
}

/// Errors from HITL transports.
#[derive(Debug, thiserror::Error)]
pub enum HitlError {
    #[error("transport error: {0}")]
    Transport(String),

    #[error("request expired")]
    Expired,

    #[error("human declined to respond")]
    Declined,

    #[error("parse error: {0}")]
    Parse(String),
}

/// GitHub comment transport — listens for `/looper` slash commands on PRs.
pub struct GitHubCommentTransport {
    /// The gateway to use for checking comments.
    /// In practice this would be the looper-github Gateway.
    gateway: Option<Arc<dyn GitHubGateway>>,
}

/// Trait abstracting GitHub operations needed for HITL comment detection.
#[async_trait]
pub trait GitHubGateway: Send + Sync {
    /// List comments on an issue/PR after a given timestamp.
    async fn list_comments_after(
        &self,
        owner: &str,
        repo: &str,
        issue: u64,
        after: &str,
    ) -> Result<Vec<Comment>, HitlError>;
}

/// A comment on a GitHub issue/PR.
#[derive(Debug, Clone)]
pub struct Comment {
    pub id: u64,
    pub author: String,
    pub body: String,
    pub created_at: String,
}

use std::sync::Arc;

impl GitHubCommentTransport {
    /// Create a new GitHub comment transport.
    pub fn new(gateway: Option<Arc<dyn GitHubGateway>>) -> Self {
        Self { gateway }
    }

    /// Parse a HITL answer from comment text.
    ///
    /// Recognized patterns:
    /// - `/looper retry`
    /// - `/looper merge`
    /// - `/looper skip`
    /// - `/looper abort`
    /// - `/looper <free text>`
    pub fn parse_answer(body: &str) -> Option<HitlDecision> {
        let trimmed = body.trim();
        let cmd = trimmed.strip_prefix("/looper ")?;
        let first_word = cmd.split_whitespace().next()?;
        match first_word {
            "retry" => Some(HitlDecision::Retry),
            "merge" => Some(HitlDecision::Merge),
            "skip" => Some(HitlDecision::Skip),
            "abort" => Some(HitlDecision::Abort),
            _ => Some(HitlDecision::Custom(cmd.to_string())),
        }
    }
}

#[async_trait]
impl HitlTransport for GitHubCommentTransport {
    async fn request_decision(&self, _request: &HitlRequest) -> Result<HitlDecision, HitlError> {
        // TODO: Post PR comment with HITL ask marker, then poll for response
        Err(HitlError::Transport("not yet implemented".into()))
    }

    async fn check_response(&self, _request_id: &str) -> Result<Option<HitlDecision>, HitlError> {
        // TODO: Check for new comments after the ask marker
        Err(HitlError::Transport("not yet implemented".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_retry() {
        assert!(matches!(GitHubCommentTransport::parse_answer("/looper retry"), Some(HitlDecision::Retry)));
    }

    #[test]
    fn parse_merge() {
        assert!(matches!(GitHubCommentTransport::parse_answer("/looper merge"), Some(HitlDecision::Merge)));
    }

    #[test]
    fn parse_skip() {
        assert!(matches!(GitHubCommentTransport::parse_answer("/looper skip"), Some(HitlDecision::Skip)));
    }

    #[test]
    fn parse_abort() {
        assert!(matches!(GitHubCommentTransport::parse_answer("/looper abort"), Some(HitlDecision::Abort)));
    }

    #[test]
    fn parse_custom() {
        match GitHubCommentTransport::parse_answer("/looper please fix the lint errors") {
            Some(HitlDecision::Custom(text)) => assert!(text.contains("lint")),
            _ => panic!("expected custom"),
        }
    }

    #[test]
    fn parse_non_command_returns_none() {
        assert!(GitHubCommentTransport::parse_answer("just a regular comment").is_none());
    }

    #[test]
    fn parse_no_looper_prefix() {
        assert!(GitHubCommentTransport::parse_answer("/other retry").is_none());
    }

    #[test]
    fn decision_display() {
        assert_eq!(HitlDecision::Retry.to_string(), "retry");
        assert_eq!(HitlDecision::Merge.to_string(), "merge");
        assert_eq!(HitlDecision::Skip.to_string(), "skip");
        assert_eq!(HitlDecision::Abort.to_string(), "abort");
        assert_eq!(HitlDecision::Custom("fix lint".into()).to_string(), "custom: fix lint");
    }

    #[test]
    fn reason_display() {
        assert_eq!(HitlReason::MergeConflict.to_string(), "merge_conflict");
        assert_eq!(HitlReason::FailingCi.to_string(), "failing_ci");
    }
}
