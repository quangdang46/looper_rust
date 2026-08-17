//! Dashboard compatibility routes.
//!
//! The Go original serves the dashboard at `/api/v1/loops` (flat), but our Rust
//! API is project-scoped (`/api/projects/{name}/loops/{seq}`).  This module
//! adds thin wrappers that translate flat selectors into project-scoped calls
//! so the React dashboard works without modification.

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    response::sse::{Event, Sse},
    routing::{get, post},
    Json, Router,
};
use futures::stream::Stream;
use serde::{Deserialize, Serialize};

use crate::envelope::Envelope;
use crate::error::ApiError;
use crate::routes::AppState;
use crate::types::internal_error;

// ---------------------------------------------------------------------------
// Types the dashboard expects (camelCase for JSON)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct SelectorParams {
    pub status: Option<String>,
    pub project_id: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Deserialize)]
pub struct RetryBody {
    pub mode: Option<String>,
    pub reset_attempts: Option<bool>,
    pub discard_worktree_changes: Option<bool>,
}

#[derive(Deserialize)]
pub struct RespondBody {
    pub answer: String,
}

#[derive(Serialize)]
pub struct StatusResponse {
    pub service: ServiceInfo,
    pub scheduler: SchedulerInfo,
    pub loops: std::collections::HashMap<String, LoopRoleCounts>,
    pub storage: StorageInfo,
    pub agent: AgentInfo,
}

#[derive(Serialize)]
pub struct ServiceInfo {
    pub healthy: bool,
    pub version: String,
}

#[derive(Serialize)]
pub struct SchedulerInfo {
    pub healthy: bool,
    pub queued_items: i64,
    pub active_runs: i64,
}

#[derive(Serialize, Default)]
pub struct LoopRoleCounts {
    pub queued: i64,
    pub running: i64,
    pub waiting: i64,
    pub paused: i64,
    pub failed: i64,
    pub terminated: i64,
    pub stopped: i64,
}

#[derive(Serialize)]
pub struct StorageInfo {
    pub healthy: bool,
    pub mode: String,
}

#[derive(Serialize)]
pub struct AgentInfo {
    pub vendor: Option<String>,
}

#[derive(Serialize)]
pub struct LoopItem {
    pub id: String,
    pub seq: i64,
    #[serde(rename = "projectId")]
    pub project_id: String,
    #[serde(rename = "type")]
    pub loop_type: String,
    #[serde(rename = "targetType")]
    pub target_type: String,
    pub status: String,
    #[serde(rename = "displayStatus")]
    pub display_status: Option<String>,
    pub attempts: Option<i32>,
    #[serde(rename = "maxAttempts")]
    pub max_attempts: Option<i32>,
    #[serde(rename = "lastFailureKind")]
    pub last_failure_kind: Option<String>,
    #[serde(rename = "lastFailureReason")]
    pub last_failure_reason: Option<String>,
    #[serde(rename = "resumePolicy")]
    pub resume_policy: Option<String>,
    #[serde(rename = "lastRunAt")]
    pub last_run_at: Option<String>,
    #[serde(rename = "nextRunAt")]
    pub next_run_at: Option<String>,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
}

#[derive(Serialize)]
pub struct LoopsList {
    pub items: Vec<LoopItem>,
    pub total: i64,
}

#[derive(Serialize)]
pub struct ProjectItem {
    pub id: String,
    pub name: String,
    #[serde(rename = "repoPath")]
    pub repo_path: String,
    #[serde(rename = "baseBranch")]
    pub base_branch: String,
    pub provider: String,
    pub archived: bool,
    pub repo: Option<String>,
    #[serde(rename = "repoUrl")]
    pub repo_url: Option<String>,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
}

#[derive(Serialize)]
pub struct ProjectsList {
    pub items: Vec<ProjectItem>,
}

#[derive(Serialize)]
pub struct ActiveRunItem {
    pub seq: i64,
    #[serde(rename = "loopId")]
    pub loop_id: String,
    #[serde(rename = "projectId")]
    pub project_id: String,
    #[serde(rename = "type")]
    pub loop_type: String,
    pub status: String,
    #[serde(rename = "loopStatus")]
    pub loop_status: String,
    #[serde(rename = "displayStatus")]
    pub display_status: String,
    pub target: serde_json::Value,
}

#[derive(Serialize)]
pub struct ActiveRunsList {
    pub items: Vec<ActiveRunItem>,
}

#[derive(Serialize)]
pub struct AgentModelEntry {
    pub id: String,
    pub label: String,
    pub source: String,
}

#[derive(Serialize)]
pub struct AgentModelsData {
    pub vendor: String,
    pub models: Vec<AgentModelEntry>,
    pub sources: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Route builder
// ---------------------------------------------------------------------------

pub fn dashboard_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/healthz", get(healthz))
        .route("/api/status", get(status))
        .route("/api/runs/active", get(runs_active))
        .route("/api/runs/active/{selector}/stop", post(stop_active_run))
        .route("/api/loops", get(list_loops_flat))
        .route("/api/loops/{selector}", get(get_loop_flat))
        .route("/api/loops/{selector}/start", post(start_loop_flat))
        .route("/api/loops/{selector}/pause", post(pause_loop_flat))
        .route("/api/loops/{selector}/respond", post(respond_loop))
        .route("/api/loops/{selector}/retry", post(retry_loop))
        .route("/api/loops/{selector}/worktree", get(loop_worktree))
        .route("/api/loops/{selector}/takeover", post(takeover_loop))
        .route("/api/loops/{selector}/handback", post(handback_loop))
        .route("/api/loops/{selector}/logs", get(loop_logs))
        .route("/api/agent/models", get(agent_models))
        .route("/api/dashboard/bootstrap/exchange", post(bootstrap_exchange))
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn healthz(State(state): State<Arc<AppState>>) -> Json<Envelope<serde_json::Value>> {
    Json(Envelope::success(serde_json::json!({
        "healthy": true,
        "startedAt": format!("{:?}", state.started_at),
    })))
}

async fn status(State(state): State<Arc<AppState>>) -> Json<Envelope<StatusResponse>> {
    let repos = state.ctx.state.repos();
    let all_loops = repos.loops.list().unwrap_or_default();
    let version = env!("CARGO_PKG_VERSION").to_string();

    let mut loop_counts: std::collections::HashMap<String, LoopRoleCounts> = std::collections::HashMap::new();
    for l in &all_loops {
        let counts = loop_counts.entry(l.project_id.clone()).or_default();
        match l.status.as_str() {
            "queued" => counts.queued += 1,
            "running" => counts.running += 1,
            "waiting" | "idle" => counts.waiting += 1,
            "paused" => counts.paused += 1,
            "failed" => counts.failed += 1,
            "terminated" | "completed" => counts.terminated += 1,
            "stopped" => counts.stopped += 1,
            _ => {}
        }
    }

    Json(Envelope::success(StatusResponse {
        service: ServiceInfo { healthy: true, version },
        scheduler: SchedulerInfo { healthy: true, queued_items: 0, active_runs: 0 },
        loops: loop_counts,
        storage: StorageInfo { healthy: true, mode: "sqlite".into() },
        agent: AgentInfo { vendor: None },
    }))
}

async fn list_loops_flat(
    State(state): State<Arc<AppState>>,
    Query(params): Query<SelectorParams>,
) -> Json<Envelope<LoopsList>> {
    let repos = state.ctx.state.repos();
    let all_loops = repos.loops.list().unwrap_or_default();
    let limit = params.limit.unwrap_or(25) as usize;
    let offset = params.offset.unwrap_or(0) as usize;

    let items: Vec<LoopItem> = all_loops
        .into_iter()
        .filter(|r| {
            if let Some(ref pid) = params.project_id {
                if &r.project_id != pid {
                    return false;
                }
            }
            if let Some(ref status_filter) = params.status {
                if &r.status != status_filter {
                    return false;
                }
            }
            true
        })
        .skip(offset)
        .take(limit)
        .map(record_to_loop_item)
        .collect();

    let total = items.len() as i64;
    Json(Envelope::success(LoopsList { items, total }))
}

async fn get_loop_flat(
    State(state): State<Arc<AppState>>,
    Path(selector): Path<String>,
) -> Result<Json<Envelope<LoopItem>>, ApiError> {
    let (project, seq) = parse_selector(&selector)?;
    let repos = state.ctx.state.repos();
    let all_loops = repos.loops.list().map_err(internal_error)?;
    let record = all_loops
        .into_iter()
        .find(|r| r.project_id == project && r.seq == seq)
        .ok_or_else(|| ApiError::not_found(format!("loop {project}:{seq} not found")))?;

    Ok(Json(Envelope::success(record_to_loop_item(record))))
}

async fn start_loop_flat(
    State(_state): State<Arc<AppState>>,
    Path(_selector): Path<String>,
) -> Json<Envelope<LoopItem>> {
    // TODO: implement resume logic
    Json(Envelope::success(LoopItem {
        id: "stub:0".into(),
        seq: 0,
        project_id: "stub".into(),
        loop_type: "planner".into(),
        target_type: "project".into(),
        status: "idle".into(),
        display_status: None,
        attempts: None,
        max_attempts: None,
        last_failure_kind: None,
        last_failure_reason: None,
        resume_policy: None,
        last_run_at: None,
        next_run_at: None,
        created_at: "".into(),
        updated_at: "".into(),
    }))
}

async fn pause_loop_flat(
    State(_state): State<Arc<AppState>>,
    Path(_selector): Path<String>,
) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "paused" }))
}

async fn respond_loop(
    State(_state): State<Arc<AppState>>,
    Path(_selector): Path<String>,
    Json(_body): Json<RespondBody>,
) -> Json<Envelope<serde_json::Value>> {
    Json(Envelope::success(serde_json::json!({ "supported": false })))
}

async fn retry_loop(
    State(_state): State<Arc<AppState>>,
    Path(_selector): Path<String>,
    Json(_body): Json<RetryBody>,
) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "mode": "retry", "resetAttempts": true }))
}

async fn loop_worktree(
    State(_state): State<Arc<AppState>>,
    Path(_selector): Path<String>,
) -> Json<Envelope<serde_json::Value>> {
    Json(Envelope::success(serde_json::json!({
        "present": false,
        "managed": false,
        "supportsClearUnusablePath": false,
    })))
}

async fn takeover_loop(
    State(_state): State<Arc<AppState>>,
    Path(_selector): Path<String>,
) -> Json<Envelope<serde_json::Value>> {
    Json(Envelope::success(serde_json::json!({
        "supported": false,
        "message": "Takeover not yet implemented",
    })))
}

async fn handback_loop(
    State(_state): State<Arc<AppState>>,
    Path(_selector): Path<String>,
) -> Json<Envelope<serde_json::Value>> {
    Json(Envelope::success(serde_json::json!({ "supported": false })))
}

async fn loop_logs(
    State(_state): State<Arc<AppState>>,
    Path(_selector): Path<String>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let _follow = params.get("follow").map(|v| v == "1").unwrap_or(false);
    let stream = futures::stream::once(async {
        Ok(Event::default().data(r#"{"content":"Log streaming not yet connected","status":"placeholder"}"#))
    });
    Sse::new(stream)
}

async fn runs_active(State(state): State<Arc<AppState>>) -> Json<Envelope<ActiveRunsList>> {
    let repos = state.ctx.state.repos();
    let all_loops = repos.loops.list().unwrap_or_default();
    let items: Vec<ActiveRunItem> = all_loops
        .into_iter()
        .filter(|r| r.status == "running")
        .map(|r| ActiveRunItem {
            seq: r.seq,
            loop_id: format!("{}:{}", r.project_id, r.seq),
            project_id: r.project_id,
            loop_type: r.r#type.clone(),
            status: r.status.clone(),
            loop_status: r.status,
            display_status: "running".into(),
            target: serde_json::json!({ "type": "project" }),
        })
        .collect();
    Json(Envelope::success(ActiveRunsList { items }))
}

async fn stop_active_run(State(_state): State<Arc<AppState>>, Path(selector): Path<String>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "stopped": true, "loopId": selector }))
}

async fn agent_models(
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Json<Envelope<AgentModelsData>> {
    let vendor = params.get("vendor").cloned().unwrap_or_else(|| "claude".into());
    let models = match vendor.as_str() {
        "claude" | "claude-code" => vec![
            AgentModelEntry {
                id: "claude-sonnet-4-20250514".into(),
                label: "Claude Sonnet 4".into(),
                source: "static".into(),
            },
            AgentModelEntry {
                id: "claude-opus-4-20250514".into(),
                label: "Claude Opus 4".into(),
                source: "static".into(),
            },
            AgentModelEntry {
                id: "claude-haiku-4-5-20251001".into(),
                label: "Claude Haiku 4.5".into(),
                source: "static".into(),
            },
        ],
        "openai" | "codex" => vec![
            AgentModelEntry { id: "gpt-4o".into(), label: "GPT-4o".into(), source: "static".into() },
            AgentModelEntry { id: "o3".into(), label: "o3".into(), source: "static".into() },
        ],
        "gemini" => vec![AgentModelEntry {
            id: "gemini-2.5-pro".into(),
            label: "Gemini 2.5 Pro".into(),
            source: "static".into(),
        }],
        _ => vec![],
    };
    Json(Envelope::success(AgentModelsData {
        vendor,
        models,
        sources: serde_json::json!({ "static": true, "probe": "skipped" }),
    }))
}

async fn bootstrap_exchange(Json(body): Json<serde_json::Value>) -> Json<Envelope<serde_json::Value>> {
    let _code = body.get("code").and_then(|v| v.as_str()).unwrap_or("");
    Json(Envelope::success(serde_json::json!({ "token": "local-dashboard-token" })))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert a `LoopRecord` to a dashboard-facing `LoopItem`.
fn record_to_loop_item(r: looper_storage::record::LoopRecord) -> LoopItem {
    LoopItem {
        id: format!("{}:{}", r.project_id, r.seq),
        seq: r.seq,
        project_id: r.project_id,
        loop_type: r.r#type,
        target_type: r.target_type,
        status: r.status,
        display_status: None,
        attempts: None,
        max_attempts: None,
        last_failure_kind: None,
        last_failure_reason: None,
        resume_policy: None,
        last_run_at: r.last_run_at,
        next_run_at: r.next_run_at,
        created_at: r.created_at,
        updated_at: r.updated_at,
    }
}

/// Parse a loop selector like `"my-project:3"` or `"3"` into (project, seq).
fn parse_selector(selector: &str) -> Result<(String, i64), ApiError> {
    if let Some((proj, seq_str)) = selector.split_once(':') {
        let seq: i64 =
            seq_str.parse().map_err(|_| ApiError::bad_request(format!("invalid seq in selector: {selector}")))?;
        Ok((proj.to_string(), seq))
    } else {
        Err(ApiError::bad_request(format!("selector must be 'project:seq', got: {selector}")))
    }
}
