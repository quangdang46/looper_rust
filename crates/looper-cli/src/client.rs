use std::collections::HashMap;

use reqwest::Client;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::CliError;

// ---------------------------------------------------------------------------
// Envelope types mirroring looper-api responses
// ---------------------------------------------------------------------------

/// Generic API envelope returned by all looper API endpoints.
#[derive(Debug, Deserialize)]
pub struct Envelope<T> {
    pub ok: bool,
    pub data: Option<T>,
    pub error: Option<ErrorInfo>,
}

#[derive(Debug, Deserialize)]
pub struct ErrorInfo {
    pub code: String,
    pub message: String,
}

// ---------------------------------------------------------------------------
// Response types (mirror looper-api routes.rs types)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub uptime_seconds: u64,
    pub version: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct VersionResponse {
    pub version: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ProjectSummary {
    pub name: String,
    pub path: Option<String>,
    #[serde(default)]
    pub repo_url: Option<String>,
    #[serde(default)]
    pub default_branch: Option<String>,
    pub schedule: Option<String>,
    pub enabled: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct LoopSummary {
    pub project_name: String,
    pub seq: i64,
    pub loop_type: String,
    pub status: String,
    pub run_statuses: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct LoopDetail {
    pub project_name: String,
    pub seq: i64,
    pub loop_type: String,
    pub status: String,
    pub target: Option<String>,
    pub metadata: Option<Value>,
    pub runs: Vec<RunSummary>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RunSummary {
    pub run_id: String,
    pub loop_seq: i64,
    pub project_name: String,
    pub step_name: String,
    pub agent_vendor: String,
    pub status: String,
    pub created_at: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RunDetail {
    pub run_id: String,
    pub loop_seq: i64,
    pub project_name: String,
    pub step_name: String,
    pub agent_vendor: String,
    pub model: Option<String>,
    pub status: String,
    pub exit_code: Option<i32>,
    pub output_truncated: Option<bool>,
    pub native_session_id: Option<String>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
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

#[derive(Debug, Deserialize, Serialize)]
pub struct EventLogResponse {
    pub id: String,
    pub timestamp: String,
    pub event_type: String,
    pub actor: String,
    pub project_name: Option<String>,
    pub loop_seq: Option<i64>,
    pub run_id: Option<String>,
    pub details: Option<Value>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct LockResponse {
    pub id: String,
    pub project_name: Option<String>,
    pub resource: String,
    pub holder: String,
    pub acquired_at: String,
    pub expires_at: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ConfigResponse {
    #[allow(dead_code)]
    pub server: Option<Value>,
    #[allow(dead_code)]
    pub storage: Option<Value>,
    #[allow(dead_code)]
    pub agent: Option<Value>,
    #[allow(dead_code)]
    pub logging: Option<Value>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AgentConfigResponse {
    pub project_name: String,
    pub agent_vendor: String,
    pub model: Option<String>,
    pub max_execution_seconds: Option<u64>,
    pub max_idle_seconds: Option<u64>,
    pub env_overrides: HashMap<String, String>,
}

// ---------------------------------------------------------------------------
// Request types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct AddProjectInput {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct CreateLoopInput {
    pub loop_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct StartRunInput {
    pub run_id: String,
    pub step_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_vendor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct EnqueueInput {
    pub queue_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loop_seq: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct AcquireLockInput {
    pub resource: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl_secs: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct PaginationParams {
    #[serde(default)]
    pub offset: u64,
    #[serde(default = "default_limit")]
    pub limit: u64,
}

fn default_limit() -> u64 { 20 }

// ---------------------------------------------------------------------------
// DaemonAPIClient
// ---------------------------------------------------------------------------

/// HTTP client that talks to a running looper daemon over the local API.
pub struct DaemonAPIClient {
    base_url: String,
    token: Option<String>,
    inner: Client,
}

impl DaemonAPIClient {
    /// Create a new client connecting to the given base URL (e.g. "http://127.0.0.1:8080").
    pub fn new(base_url: String, token: Option<String>) -> Self {
        Self {
            base_url,
            token,
            inner: Client::new(),
        }
    }

    fn request_builder(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let url = format!("{}{}", self.base_url, path);
        let mut req = self.inner.request(method, &url);
        if let Some(t) = &self.token {
            req = req.header("Authorization", format!("Bearer {t}"));
        }
        req
    }

    /// GET and parse Envelope<T>.
    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, CliError> {
        let resp = self.request_builder(reqwest::Method::GET, path)
            .send()
            .await?;
        let env: Envelope<T> = resp.json().await?;
        env.into_result()
    }

    /// POST and parse Envelope<T>.
    async fn post<T: DeserializeOwned, B: Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, CliError> {
        let resp = self.request_builder(reqwest::Method::POST, path)
            .json(body)
            .send()
            .await?;
        let env: Envelope<T> = resp.json().await?;
        env.into_result()
    }

    /// POST and ignore the response body (works for `Envelope<()>` and similar).
    async fn post_unit<B: Serialize>(&self, path: &str, body: &B) -> Result<(), CliError> {
        let resp = self.request_builder(reqwest::Method::POST, path)
            .json(body)
            .send()
            .await?;
        let env: Envelope<()> = resp.json().await?;
        into_unit_result(env)
    }

    /// DELETE and parse Envelope<()> — empty body is success.
    async fn delete_unit(&self, path: &str) -> Result<(), CliError> {
        let resp = self.request_builder(reqwest::Method::DELETE, path)
            .send()
            .await?;
        let env: Envelope<()> = resp.json().await?;
        into_unit_result(env)
    }

    // -----------------------------------------------------------------------
    // Health & version
    // -----------------------------------------------------------------------

    /// Ping the daemon — returns None if unreachable.
    pub async fn health(&self) -> Result<HealthResponse, CliError> {
        self.get("/health").await
    }

    pub async fn server_version(&self) -> Result<VersionResponse, CliError> {
        self.get("/version").await
    }

    /// Attempt a quick connectivity check.
    pub async fn ping(&self) -> bool {
        self.health().await.is_ok()
    }

    // -----------------------------------------------------------------------
    // Projects
    // -----------------------------------------------------------------------

    pub async fn list_projects(&self) -> Result<Vec<ProjectSummary>, CliError> {
        self.get("/api/projects").await
    }

    pub async fn add_project(&self, input: &AddProjectInput) -> Result<ProjectSummary, CliError> {
        self.post("/api/projects", input).await
    }

    pub async fn get_project(&self, name: &str) -> Result<ProjectSummary, CliError> {
        self.get(&format!("/api/projects/{name}")).await
    }

    pub async fn remove_project(&self, name: &str) -> Result<(), CliError> {
        self.delete_unit(&format!("/api/projects/{name}")).await
    }

    pub async fn sync_project(&self, name: &str) -> Result<ProjectSummary, CliError> {
        self.post(&format!("/api/projects/{name}/sync"), &serde_json::Map::new()).await
    }

    // -----------------------------------------------------------------------
    // Loops
    // -----------------------------------------------------------------------

    pub async fn list_loops(
        &self,
        project: &str,
        offset: u64,
        limit: u64,
    ) -> Result<Vec<LoopSummary>, CliError> {
        self.get(&format!("/api/projects/{project}/loops?offset={offset}&limit={limit}")).await
    }

    pub async fn create_loop(
        &self,
        project: &str,
        input: &CreateLoopInput,
    ) -> Result<LoopDetail, CliError> {
        self.post(&format!("/api/projects/{project}/loops"), input).await
    }

    pub async fn get_loop(&self, project: &str, seq: i64) -> Result<LoopDetail, CliError> {
        self.get(&format!("/api/projects/{project}/loops/{seq}")).await
    }

    pub async fn pause_loop(&self, project: &str, seq: i64) -> Result<(), CliError> {
        self.post_unit(
            &format!("/api/projects/{project}/loops/{seq}/pause"),
            &serde_json::Map::new(),
        )
        .await
    }

    pub async fn resume_loop(&self, project: &str, seq: i64) -> Result<(), CliError> {
        self.post_unit(
            &format!("/api/projects/{project}/loops/{seq}/resume"),
            &serde_json::Map::new(),
        )
        .await
    }

    pub async fn terminate_loop(&self, project: &str, seq: i64) -> Result<(), CliError> {
        self.post_unit(
            &format!("/api/projects/{project}/loops/{seq}/terminate"),
            &serde_json::Map::new(),
        )
        .await
    }

    // -----------------------------------------------------------------------
    // Runs
    // -----------------------------------------------------------------------

    pub async fn list_runs(
        &self,
        project: &str,
        seq: i64,
        offset: u64,
        limit: u64,
    ) -> Result<Vec<RunSummary>, CliError> {
        self.get(&format!(
            "/api/projects/{project}/loops/{seq}/runs?offset={offset}&limit={limit}",
        )).await
    }

    pub async fn start_run(
        &self,
        project: &str,
        seq: i64,
        input: &StartRunInput,
    ) -> Result<RunDetail, CliError> {
        self.post(&format!("/api/projects/{project}/loops/{seq}/runs"), input).await
    }

    pub async fn get_run(
        &self,
        project: &str,
        seq: i64,
        run_id: &str,
    ) -> Result<RunDetail, CliError> {
        self.get(&format!("/api/projects/{project}/loops/{seq}/runs/{run_id}")).await
    }

    pub async fn cancel_run(&self, project: &str, seq: i64) -> Result<(), CliError> {
        self.post_unit(
            &format!("/api/projects/{project}/loops/{seq}/runs/cancel"),
            &serde_json::Map::new(),
        )
        .await
    }

    // -----------------------------------------------------------------------
    // Queue
    // -----------------------------------------------------------------------

    pub async fn list_queue(
        &self,
        project: &str,
        offset: u64,
        limit: u64,
    ) -> Result<Vec<QueueItemResponse>, CliError> {
        self.get(&format!("/api/projects/{project}/queue?offset={offset}&limit={limit}")).await
    }

    pub async fn enqueue(
        &self,
        project: &str,
        input: &EnqueueInput,
    ) -> Result<QueueItemResponse, CliError> {
        self.post(
            &format!("/api/projects/{project}/queue/enqueue"),
            input,
        )
        .await
    }

    pub async fn dequeue(&self, project: &str, item_id: &str) -> Result<(), CliError> {
        self.delete_unit(&format!("/api/projects/{project}/queue/{item_id}")).await
    }

    // -----------------------------------------------------------------------
    // Events
    // -----------------------------------------------------------------------

    pub async fn list_events(
        &self,
        project: &str,
        offset: u64,
        limit: u64,
    ) -> Result<Vec<EventLogResponse>, CliError> {
        self.get(&format!("/api/projects/{project}/events?offset={offset}&limit={limit}")).await
    }

    // -----------------------------------------------------------------------
    // Locks
    // -----------------------------------------------------------------------

    pub async fn list_locks(&self) -> Result<Vec<LockResponse>, CliError> {
        self.get("/api/locks").await
    }

    pub async fn acquire_lock(&self, input: &AcquireLockInput) -> Result<LockResponse, CliError> {
        self.post("/api/locks", input).await
    }

    pub async fn release_lock(&self, key: &str) -> Result<(), CliError> {
        self.delete_unit(&format!("/api/locks/{key}")).await
    }

    // -----------------------------------------------------------------------
    // Config
    // -----------------------------------------------------------------------

    pub async fn get_config(&self) -> Result<ConfigResponse, CliError> {
        self.get("/api/config").await
    }

    pub async fn get_agent_config(&self, project: &str) -> Result<AgentConfigResponse, CliError> {
        self.get(&format!("/api/config/agent/{project}")).await
    }

    // -----------------------------------------------------------------------
    // Daemon lifecycle
    // -----------------------------------------------------------------------

    pub async fn api_shutdown(&self) -> Result<(), CliError> {
        self.post_unit("/shutdown", &serde_json::Map::new()).await
    }

    pub async fn api_reload(&self) -> Result<(), CliError> {
        self.post_unit("/reload", &serde_json::Map::new()).await
    }

    // -----------------------------------------------------------------------
    // Worktree cleanup
    // -----------------------------------------------------------------------

    pub async fn worktree_cleanup(&self, input: &crate::commands::worktree::WorktreeCleanupInput) -> Result<crate::commands::worktree::WorktreeCleanupResult, CliError> {
        self.post("/api/worktree/cleanup", input).await
    }
}

// ---------------------------------------------------------------------------
// Envelope result extraction
// ---------------------------------------------------------------------------

impl<T> Envelope<T> {
    pub fn into_result(self) -> Result<T, CliError> {
        if self.ok {
            // For unit-typed responses (e.g. pause, terminate, shutdown), the
            // API legitimately returns `data: null`. Treat that as success.
            self.data.ok_or_else(|| CliError::api("Internal", "missing data in success response"))
        } else {
            let info = self.error.unwrap_or(ErrorInfo {
                code: "Unknown".into(),
                message: "no error details".into(),
            });
            Err(CliError::api(info.code, info.message))
        }
    }
}

/// Helper for endpoints that return `Envelope<()>` (success-with-empty-body).
pub fn into_unit_result(env: Envelope<()>) -> Result<(), CliError> {
    if env.ok {
        Ok(())
    } else {
        let info = env.error.unwrap_or(ErrorInfo {
            code: "Unknown".into(),
            message: "no error details".into(),
        });
        Err(CliError::api(info.code, info.message))
    }
}
