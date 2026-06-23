use std::fmt;
use std::sync::Arc;

use axum::extract::Request;
use axum::response::{IntoResponse, Response};
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use async_trait::async_trait;

use crate::envelope::Envelope;
use crate::error::ApiError;

// ---------------------------------------------------------------------------
// Context — shared request-scoped state
// ---------------------------------------------------------------------------

/// Shared context injected as axum `Extension` into every handler.
pub struct Context {
    pub config: Arc<looper_config::Config>,
    pub state: Arc<dyn RuntimeState>,
    pub projects: Arc<dyn ProjectService>,
}

// ---------------------------------------------------------------------------
// RuntimeState — storage & scheduler bridge
// ---------------------------------------------------------------------------

/// Abstract interface to runtime storage and scheduler control.
///
/// The daemon implements this trait and passes it to the API server so
/// handlers can query repositories and issue lifecycle commands without
/// importing concrete scheduler types.
#[async_trait]
pub trait RuntimeState: Send + Sync {
    /// Access the repositories container.
    fn repos(&self) -> &looper_storage::Repositories;

    /// Access the event log service.
    fn event_log(&self) -> &looper_storage::EventLog;

    /// Signal the scheduler to stop a specific loop run.
    async fn stop_loop(&self, project_name: &str, loop_seq: i64) -> Result<(), ApiError>;

    /// Signal the scheduler to close / finalize a loop.
    async fn close_loop(&self, project_name: &str, loop_seq: i64) -> Result<(), ApiError>;

    /// Stop all active runs for a project.
    async fn stop_all(&self, project_name: &str) -> Result<(), ApiError>;

    /// Trigger the reviewer repair flow.
    async fn repair_reviewer(&self, project_name: &str) -> Result<(), ApiError>;

    /// Trigger an immediate scheduler tick (for manual / debug endpoints).
    async fn trigger_scheduler_tick(&self);
}

// ---------------------------------------------------------------------------
// ProjectService — project lifecycle
// ---------------------------------------------------------------------------

/// Abstract interface for project CRUD used by the API layer.
#[async_trait]
pub trait ProjectService: Send + Sync {
    /// Add a new project (validates, creates worktree, upserts DB).
    async fn add(&self, input: AddProjectInput) -> Result<ProjectSummary, ApiError>;

    /// Remove a project by name (archives, terminates loops, cancels queue).
    async fn remove(&self, name: &str) -> Result<(), ApiError>;

    /// List all projects.
    async fn list(&self) -> Result<Vec<ProjectSummary>, ApiError>;

    /// Sync a project's worktree / PR discovery.
    async fn sync(&self, name: &str) -> Result<ProjectSummary, ApiError>;
}

// ---------------------------------------------------------------------------
// Handler type alias
// ---------------------------------------------------------------------------

/// An axum handler function that receives shared context.
pub type Handler = Box<dyn Fn(Arc<Context>, Request) -> BoxFuture<'static, Response> + Send + Sync>;

// ---------------------------------------------------------------------------
// ApiServiceError — bridges service errors to API errors
// ---------------------------------------------------------------------------

/// Lightweight error type that handler functions use internally to express
/// business-logic failures without coupling to concrete service crates.
#[derive(Debug)]
pub enum ApiServiceError<E: fmt::Display> {
    NotFound(String),
    Conflict(String),
    BadRequest(String),
    Validation(String),
    Internal(String),
    Service(E),
}

impl<E: fmt::Display> From<E> for ApiServiceError<E> {
    fn from(e: E) -> Self {
        Self::Service(e)
    }
}

// ---------------------------------------------------------------------------
// Request types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct AddProjectInput {
    pub name: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub repo_url: Option<String>,
    #[serde(default)]
    pub default_branch: Option<String>,
    #[serde(default)]
    pub schedule: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub archive_filter: Option<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub struct StartRunInput {
    pub run_id: String,
    pub step_name: String,
    pub agent_vendor: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub config_override: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct CreateLoopInput {
    pub loop_type: String,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct EnqueueInput {
    pub queue_type: String,
    #[serde(default)]
    pub loop_seq: Option<i64>,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub priority: Option<i32>,
    #[serde(default)]
    pub payload: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct AcquireLockInput {
    pub resource: String,
    #[serde(default)]
    pub ttl_secs: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct PaginationParams {
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

fn default_limit() -> i64 {
    50
}

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub uptime_seconds: u64,
    pub version: String,
}

#[derive(Debug, Serialize)]
pub struct VersionResponse {
    pub version: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct ProjectSummary {
    pub name: String,
    pub path: String,
    pub repo_url: Option<String>,
    pub default_branch: String,
    pub schedule: String,
    pub enabled: bool,
    pub archive_filter: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct LoopSummary {
    pub project_name: String,
    pub seq: i64,
    pub loop_type: String,
    pub status: String,
    pub run_statuses: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize)]
pub struct LoopDetail {
    pub project_name: String,
    pub seq: i64,
    pub loop_type: String,
    pub status: String,
    pub target: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub runs: Vec<RunSummary>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct RunSummary {
    pub run_id: String,
    pub loop_seq: i64,
    pub project_name: String,
    pub step_name: String,
    pub agent_vendor: String,
    pub status: String,
    pub created_at: String,
}

#[derive(Debug, Serialize)]
pub struct RunDetail {
    pub run_id: String,
    pub loop_seq: i64,
    pub project_name: String,
    pub step_name: String,
    pub agent_vendor: String,
    pub model: Option<String>,
    pub status: String,
    pub exit_code: Option<i32>,
    pub output_truncated: Option<String>,
    pub native_session_id: Option<String>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct QueueItemResponse {
    pub id: String,
    pub project_name: String,
    pub queue_type: String,
    pub loop_seq: Option<i64>,
    pub run_id: Option<String>,
    pub status: String,
    pub priority: i32,
    pub attempts: i32,
    pub last_error: Option<String>,
    pub scheduled_not_before: Option<String>,
    pub claimed_by: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize)]
pub struct EventLogResponse {
    pub id: String,
    pub timestamp: String,
    pub event_type: String,
    pub actor: String,
    pub project_name: Option<String>,
    pub loop_seq: Option<i64>,
    pub run_id: Option<String>,
    pub details: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct LockResponse {
    pub id: String,
    pub project_name: Option<String>,
    pub resource: String,
    pub holder: String,
    pub acquired_at: String,
    pub expires_at: Option<String>,
}

/// Expose the full resolved config tree.
/// Since `looper-config::Config` already derives Serialize we can just
/// re-export it as a response type alias.
pub type ConfigResponse = looper_config::Config;

#[derive(Debug, Serialize)]
pub struct AgentConfigResponse {
    pub project_name: String,
    pub agent_vendor: String,
    pub model: Option<String>,
    pub max_execution_seconds: u64,
    pub max_idle_seconds: u64,
    pub env_overrides: std::collections::HashMap<String, String>,
}

// ---------------------------------------------------------------------------
// Helper: convert service errors to API errors
// ---------------------------------------------------------------------------

/// Helper to map any Display-able error to an internal ApiError.
pub fn internal_error<E: fmt::Display>(err: E) -> ApiError {
    ApiError::internal(err.to_string())
}

/// Helper to convert a domain-type result into an API response.
pub fn api_result<T: Serialize>(result: Result<T, ApiError>) -> Response {
    match result {
        Ok(data) => Envelope::success(data).into_response(),
        Err(err) => err.into_response(),
    }
}
