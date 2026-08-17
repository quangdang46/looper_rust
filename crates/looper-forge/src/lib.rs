//! Forge abstraction layer — unified interface for GitHub, Forgejo, and Gitea.
//!
//! Each provider implements the [`Forge`] trait with its own capabilities.
//! The [`Registry`] resolves project config to the correct provider.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

pub mod types;

pub use types::*;

/// Errors from forge operations.
#[derive(Debug, thiserror::Error)]
pub enum ForgeError {
    #[error("HTTP error: {0}")]
    Http(String),

    #[error("auth error: {0}")]
    Auth(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("rate limited, retry after {0}s")]
    RateLimited(u64),

    #[error("parse error: {0}")]
    Parse(String),

    #[error("provider error: {0}")]
    Provider(String),
}

/// Capability flags for a forge provider.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Capabilities {
    pub issues: bool,
    pub pull_requests: bool,
    pub labels: bool,
    pub assignees: bool,
    pub native_reviews: bool,
    pub review_requests: bool,
    pub auto_merge: bool,
    pub webhooks: bool,
    pub review_comment_resolution: ThreadResolution,
    pub worker_claim: WorkerClaim,
    pub review_discovery: ReviewDiscovery,
    pub review_publish: ReviewPublish,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadResolution {
    Native,
    ManualOnly,
    Disabled,
}

impl Default for ThreadResolution {
    fn default() -> Self {
        Self::Disabled
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerClaim {
    AssignSelf,
    PreAssigned,
}

impl Default for WorkerClaim {
    fn default() -> Self {
        Self::AssignSelf
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDiscovery {
    ReviewRequest,
    Label,
}

impl Default for ReviewDiscovery {
    fn default() -> Self {
        Self::ReviewRequest
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewPublish {
    NativeReview,
    CommentOnly,
}

impl Default for ReviewPublish {
    fn default() -> Self {
        Self::NativeReview
    }
}

/// Provider kind identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    GitHub,
    Forgejo,
    Gitea,
}

impl fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GitHub => write!(f, "github"),
            Self::Forgejo => write!(f, "forgejo"),
            Self::Gitea => write!(f, "gitea"),
        }
    }
}

/// Identity of the current user on a forge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Identity {
    pub login: String,
    pub id: u64,
    pub name: Option<String>,
}

/// A forge provider (GitHub, Forgejo, Gitea).
#[async_trait]
pub trait Forge: Send + Sync {
    /// Provider kind.
    fn kind(&self) -> ProviderKind;

    /// Capability flags.
    fn capabilities(&self) -> Capabilities;

    /// Get the current user's identity.
    async fn current_user(&self) -> Result<Identity, ForgeError>;

    // -- Issues --
    async fn list_issues(&self, filter: IssueFilter) -> Result<Vec<Issue>, ForgeError>;
    async fn get_issue(&self, number: u64) -> Result<Issue, ForgeError>;
    async fn create_comment(&self, number: u64, body: &str) -> Result<Comment, ForgeError>;

    // -- Pull Requests --
    async fn list_pull_requests(&self, filter: PrFilter) -> Result<Vec<PullRequest>, ForgeError>;
    async fn get_pull_request(&self, number: u64) -> Result<PullRequest, ForgeError>;
    async fn create_pull_request(&self, input: CreatePr) -> Result<PullRequest, ForgeError>;
    async fn merge_pull_request(&self, number: u64, strategy: MergeStrategy) -> Result<(), ForgeError>;

    // -- Reviews --
    async fn submit_review(&self, pr: u64, input: ReviewInput) -> Result<(), ForgeError>;
    async fn list_review_threads(&self, pr: u64) -> Result<Vec<ReviewThread>, ForgeError>;
    async fn resolve_review_thread(&self, thread_id: &str) -> Result<(), ForgeError>;

    // -- Labels --
    async fn add_label(&self, issue: u64, label: &str) -> Result<(), ForgeError>;
    async fn remove_label(&self, issue: u64, label: &str) -> Result<(), ForgeError>;
}

/// Registry that resolves project config to the correct forge provider.
pub struct Registry {
    providers: HashMap<String, Arc<dyn Forge>>,
}

impl Registry {
    pub fn new() -> Self {
        Self { providers: HashMap::new() }
    }

    pub fn register(&mut self, name: String, provider: Arc<dyn Forge>) {
        self.providers.insert(name, provider);
    }

    pub fn resolve(&self, project_provider: &str) -> Option<Arc<dyn Forge>> {
        self.providers.get(project_provider).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_default() {
        let caps = Capabilities::default();
        assert!(!caps.issues);
        assert!(!caps.pull_requests);
        assert!(!caps.auto_merge);
    }

    #[test]
    fn provider_kind_display() {
        assert_eq!(ProviderKind::GitHub.to_string(), "github");
        assert_eq!(ProviderKind::Forgejo.to_string(), "forgejo");
        assert_eq!(ProviderKind::Gitea.to_string(), "gitea");
    }

    #[test]
    fn registry_resolve() {
        // Can't test with trait objects easily, but test the registry structure
        let registry = Registry::new();
        assert!(registry.resolve("nonexistent").is_none());
    }
}
