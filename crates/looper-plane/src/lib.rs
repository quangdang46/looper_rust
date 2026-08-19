//! Plane project management provider — REST API client for Plane work-items.
//!
//! Plane is a project management tool (self-hosted or cloud). This crate implements
//! the [`looper_forge::Forge`] trait for Plane, supporting **issues only** (no PRs,
//! no reviews, no native merge).
//!
//! # Configuration
//!
//! ```toml
//! [projects.my-project.provider]
//! type = "plane"
//! base_url = "https://plane.example.com"
//! api_token = "plk_..."
//! workspace = "my-workspace"
//! project = "my-project-slug"
//! ```

use async_trait::async_trait;
use looper_forge::{
    Capabilities, Comment, CreatePr, Forge, ForgeError, Identity, Issue, IssueFilter, IssueState, MergeStrategy,
    ProviderKind, PullRequest, ReviewInput, ReviewThread,
};
use reqwest::Client;
use std::sync::Arc;

pub mod types;

use types::{issue_from_plane, PlaneIssuesResponse};

/// Plane provider configuration.
#[derive(Debug, Clone)]
pub struct PlaneConfig {
    /// Base URL of the Plane instance (e.g. `https://plane.example.com`).
    pub base_url: String,
    /// API token for authentication.
    pub api_token: String,
    /// Workspace slug.
    pub workspace: String,
    /// Project slug within the workspace.
    pub project: String,
}

/// Plane REST API client implementing the [`Forge`] trait.
///
/// Issues are mapped 1:1. Pull requests, reviews, and native merge are **not supported**
/// — Plane does not have these concepts.
pub struct PlaneForge {
    config: PlaneConfig,
    client: Client,
}

impl PlaneForge {
    /// Create a new Plane forge provider.
    pub fn new(config: PlaneConfig) -> Self {
        Self { config, client: Client::new() }
    }

    /// Create with a pre-configured HTTP client (for testing or custom middleware).
    pub fn with_client(config: PlaneConfig, client: Client) -> Self {
        Self { config, client }
    }

    /// Base API URL: `{base_url}/api/v1/workspaces/{workspace}/projects/{project}`.
    fn api_base(&self) -> String {
        format!("{}/api/v1/workspaces/{}/projects/{}", self.config.base_url, self.config.workspace, self.config.project)
    }

    /// Build an authenticated request builder.
    fn request(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, url)
            .header("x-api-key", &self.config.api_token)
            .header("Content-Type", "application/json")
    }

    /// Fetch issues from Plane REST API.
    async fn fetch_issues(&self, state: Option<&str>) -> Result<Vec<Issue>, ForgeError> {
        let mut url = format!("{}/issues/", self.api_base());
        if let Some(s) = state {
            url = format!("{}?state={}", url, s);
        }

        let resp =
            self.request(reqwest::Method::GET, &url).send().await.map_err(|e| ForgeError::Http(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(ForgeError::Http(format!("Plane API returned {}", resp.status())));
        }

        let body: PlaneIssuesResponse = resp.json().await.map_err(|e| ForgeError::Parse(e.to_string()))?;

        Ok(body.results.into_iter().map(issue_from_plane).collect())
    }
}

/// Errors specific to the Plane provider.
#[derive(Debug, thiserror::Error)]
pub enum PlaneError {
    #[error("HTTP error: {0}")]
    Http(String),

    #[error("authentication failed: {0}")]
    Auth(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("parse error: {0}")]
    Parse(String),
}

impl From<PlaneError> for ForgeError {
    fn from(e: PlaneError) -> Self {
        match e {
            PlaneError::Http(msg) => ForgeError::Http(msg),
            PlaneError::Auth(msg) => ForgeError::Auth(msg),
            PlaneError::NotFound(msg) => ForgeError::NotFound(msg),
            PlaneError::Parse(msg) => ForgeError::Parse(msg),
        }
    }
}

#[async_trait]
impl Forge for PlaneForge {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Plane
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            issues: true,
            pull_requests: false,
            labels: false,
            assignees: false,
            native_reviews: false,
            review_requests: false,
            auto_merge: false,
            webhooks: false,
            ..Default::default()
        }
    }

    async fn current_user(&self) -> Result<Identity, ForgeError> {
        let url = format!("{}/api/v1/users/me/", self.config.base_url);
        let resp =
            self.request(reqwest::Method::GET, &url).send().await.map_err(|e| ForgeError::Http(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(ForgeError::Auth(format!("Plane API returned {}", resp.status())));
        }

        let body: serde_json::Value = resp.json().await.map_err(|e| ForgeError::Parse(e.to_string()))?;

        Ok(Identity {
            login: body["email"].as_str().unwrap_or("unknown").to_string(),
            id: body["id"].as_u64().unwrap_or(0),
            name: body.get("display_name").and_then(|v| v.as_str()).map(String::from),
        })
    }

    // -- Issues --

    async fn list_issues(&self, filter: IssueFilter) -> Result<Vec<Issue>, ForgeError> {
        let state = filter.state.as_ref().map(|s| match s {
            IssueState::Open => "unstarted,started,completed",
            IssueState::Closed => "cancelled",
        });

        let mut issues = self.fetch_issues(state).await?;

        // Apply label filter client-side (Plane API may not support label filtering)
        if let Some(ref labels) = filter.labels {
            issues.retain(|i| labels.iter().any(|l| i.labels.contains(l)));
        }

        // Apply limit
        if let Some(limit) = filter.limit {
            issues.truncate(limit as usize);
        }

        Ok(issues)
    }

    async fn get_issue(&self, number: u64) -> Result<Issue, ForgeError> {
        // Plane uses identifier format "PROJECT-123" — we need to search
        let issues = self.fetch_issues(None).await?;
        issues
            .into_iter()
            .find(|i| i.number == number)
            .ok_or_else(|| ForgeError::NotFound(format!("issue #{}", number)))
    }

    async fn create_comment(&self, _number: u64, _body: &str) -> Result<Comment, ForgeError> {
        // Plane issues don't have a native comment API in the same way GitHub does.
        // Return an error indicating this operation is not supported.
        Err(ForgeError::Provider("Plane does not support issue comments via this integration".to_string()))
    }

    // -- Pull Requests (not supported) --

    async fn list_pull_requests(&self, _filter: looper_forge::PrFilter) -> Result<Vec<PullRequest>, ForgeError> {
        Err(ForgeError::Provider("Plane does not have pull requests".to_string()))
    }

    async fn get_pull_request(&self, _number: u64) -> Result<PullRequest, ForgeError> {
        Err(ForgeError::Provider("Plane does not have pull requests".to_string()))
    }

    async fn create_pull_request(&self, _input: CreatePr) -> Result<PullRequest, ForgeError> {
        Err(ForgeError::Provider("Plane does not have pull requests".to_string()))
    }

    async fn merge_pull_request(&self, _number: u64, _strategy: MergeStrategy) -> Result<(), ForgeError> {
        Err(ForgeError::Provider("Plane does not have pull requests".to_string()))
    }

    // -- Reviews (not supported) --

    async fn submit_review(&self, _pr: u64, _input: ReviewInput) -> Result<(), ForgeError> {
        Err(ForgeError::Provider("Plane does not support code reviews".to_string()))
    }

    async fn list_review_threads(&self, _pr: u64) -> Result<Vec<ReviewThread>, ForgeError> {
        Err(ForgeError::Provider("Plane does not support code reviews".to_string()))
    }

    async fn resolve_review_thread(&self, _thread_id: &str) -> Result<(), ForgeError> {
        Err(ForgeError::Provider("Plane does not support code reviews".to_string()))
    }

    // -- Labels (not supported) --

    async fn add_label(&self, _issue: u64, _label: &str) -> Result<(), ForgeError> {
        Err(ForgeError::Provider("Plane label management not supported".to_string()))
    }

    async fn remove_label(&self, _issue: u64, _label: &str) -> Result<(), ForgeError> {
        Err(ForgeError::Provider("Plane label management not supported".to_string()))
    }
}

/// Create a boxed Plane forge provider from config.
pub fn create_plane_provider(config: PlaneConfig) -> Arc<dyn Forge> {
    Arc::new(PlaneForge::new(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plane_capabilities_issues_only() {
        let forge = PlaneForge::new(PlaneConfig {
            base_url: "https://plane.example.com".to_string(),
            api_token: "test".to_string(),
            workspace: "ws".to_string(),
            project: "proj".to_string(),
        });

        let caps = forge.capabilities();
        assert!(caps.issues);
        assert!(!caps.pull_requests);
        assert!(!caps.native_reviews);
        assert!(!caps.auto_merge);
        assert!(!caps.webhooks);
    }

    #[test]
    fn plane_provider_kind() {
        let forge = PlaneForge::new(PlaneConfig {
            base_url: "https://plane.example.com".to_string(),
            api_token: "test".to_string(),
            workspace: "ws".to_string(),
            project: "proj".to_string(),
        });
        assert_eq!(forge.kind(), ProviderKind::Plane);
    }

    #[test]
    fn plane_api_base_url() {
        let forge = PlaneForge::new(PlaneConfig {
            base_url: "https://plane.example.com".to_string(),
            api_token: "test".to_string(),
            workspace: "my-workspace".to_string(),
            project: "my-project".to_string(),
        });
        assert_eq!(forge.api_base(), "https://plane.example.com/api/v1/workspaces/my-workspace/projects/my-project");
    }

    #[tokio::test]
    async fn plane_unsupported_operations_return_errors() {
        let forge = PlaneForge::new(PlaneConfig {
            base_url: "https://plane.example.com".to_string(),
            api_token: "test".to_string(),
            workspace: "ws".to_string(),
            project: "proj".to_string(),
        });

        assert!(forge.list_pull_requests(Default::default()).await.is_err());
        assert!(forge.get_pull_request(1).await.is_err());
        assert!(forge
            .submit_review(1, ReviewInput { event: looper_forge::ReviewEvent::Approve, body: None, comments: vec![] })
            .await
            .is_err());
        assert!(forge.add_label(1, "bug").await.is_err());
        assert!(forge.remove_label(1, "bug").await.is_err());
    }
}
