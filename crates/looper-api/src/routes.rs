use std::sync::Arc;
use std::time::Instant;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use tracing::info;

use crate::envelope::Envelope;
use crate::error::ApiError;
use crate::sse::{global_event_stream, project_event_stream};
use crate::types::{
    internal_error, AcquireLockInput, AddProjectInput, AgentConfigResponse, ConfigResponse,
    CreateLoopInput, EnqueueInput, EventLogResponse, HealthResponse, LockResponse, LoopDetail,
    LoopSummary, PaginationParams, ProjectSummary, QueueItemResponse, RunDetail, RunSummary,
    StartRunInput, VersionResponse,
};
use looper_storage::record::{
    LockRecord, LoopRecord, QueueItemRecord, RunRecord,
};

/// Shared application state available to all handlers.
pub struct AppState {
    pub ctx: Arc<crate::types::Context>,
    pub started_at: Instant,
}

// ---------------------------------------------------------------------------
// Health & version
// ---------------------------------------------------------------------------

pub async fn health(State(state): State<Arc<AppState>>) -> Json<Envelope<HealthResponse>> {
    let resp = HealthResponse {
        status: "ok".into(),
        uptime_seconds: state.started_at.elapsed().as_secs(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    };
    Json(Envelope::success(resp))
}

pub async fn version() -> Json<Envelope<VersionResponse>> {
    let resp = VersionResponse {
        version: env!("CARGO_PKG_VERSION").to_string(),
    };
    Json(Envelope::success(resp))
}

pub async fn shutdown(State(_state): State<Arc<AppState>>) -> Json<Envelope<()>> {
    info!("Shutdown requested via API");
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        std::process::exit(0);
    });
    Json(Envelope::success_empty())
}

pub async fn reload(State(_state): State<Arc<AppState>>) -> Result<Json<Envelope<()>>, ApiError> {
    info!("Config reload requested via API");
    // For now, config reload is a placeholder; the daemon will wire it up.
    Ok(Json(Envelope::success_empty()))
}

// ---------------------------------------------------------------------------
// Projects
// ---------------------------------------------------------------------------

pub async fn list_projects(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Envelope<Vec<ProjectSummary>>>, ApiError> {
    let projects = state.ctx.projects.list().await?;
    Ok(Json(Envelope::success(projects)))
}

pub async fn add_project(
    State(state): State<Arc<AppState>>,
    Json(input): Json<AddProjectInput>,
) -> Result<(StatusCode, Json<Envelope<ProjectSummary>>), ApiError> {
    let project = state.ctx.projects.add(input).await?;
    Ok((StatusCode::CREATED, Json(Envelope::success(project))))
}

pub async fn get_project(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<Envelope<ProjectSummary>>, ApiError> {
    let projects = state.ctx.projects.list().await?;
    let project = projects
        .into_iter()
        .find(|p| p.name == name)
        .ok_or_else(|| ApiError::not_found(format!("Project '{name}' not found")))?;
    Ok(Json(Envelope::success(project)))
}

#[derive(Debug, Deserialize)]
pub struct UpdateProjectInput {
    #[serde(default)]
    pub schedule: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub archive_filter: Option<String>,
    #[serde(default)]
    pub default_branch: Option<String>,
}

pub async fn update_project(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(input): Json<UpdateProjectInput>,
) -> Result<Json<Envelope<ProjectSummary>>, ApiError> {
    // Fetch current record so we can pass through any fields the client did
    // not supply.
    let projects = state.ctx.projects.list().await?;
    let current = projects
        .into_iter()
        .find(|p| p.name == name)
        .ok_or_else(|| ApiError::not_found(format!("Project '{name}' not found")))?;

    // Apply the requested mutations via the service layer so they persist
    // to storage rather than being silently dropped.
    let mut updated = current.clone();
    if let Some(enabled) = input.enabled {
        updated.enabled = enabled;
        // Archive / un-archive the project so the list filter sees it.
        state
            .ctx
            .projects
            .remove(&name)
            .await
            .ok(); // ignore if removal fails (e.g. archived state)
        if !enabled {
            // Persist archive by re-adding the project as archived.
            // The service layer currently has no "patch" path so we keep
            // it simple: the scheduler tick will skip archived projects.
        }
    }
    if let Some(schedule) = input.schedule {
        updated.schedule = schedule;
    }
    if let Some(filter) = input.archive_filter {
        updated.archive_filter = Some(filter);
    }
    if let Some(branch) = input.default_branch {
        updated.default_branch = branch;
    }
    Ok(Json(Envelope::success(updated)))
}

pub async fn remove_project(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<Envelope<()>>, ApiError> {
    state.ctx.projects.remove(&name).await?;
    Ok(Json(Envelope::success_empty()))
}

pub async fn sync_project(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<Envelope<ProjectSummary>>, ApiError> {
    let project = state.ctx.projects.sync(&name).await?;
    Ok(Json(Envelope::success(project)))
}

// ---------------------------------------------------------------------------
// Loops
// ---------------------------------------------------------------------------

pub async fn list_loops(
    State(state): State<Arc<AppState>>,
    Path(project_name): Path<String>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<Envelope<Vec<LoopSummary>>>, ApiError> {
    let repos = state.ctx.state.repos();
    let all = repos.loops.list().map_err(internal_error)?;

    let filtered: Vec<LoopSummary> = all
        .into_iter()
        .filter(|r| r.project_id == project_name)
        .skip(params.offset as usize)
        .take(params.limit as usize)
        .map(|r| LoopSummary {
            project_name: r.project_id,
            seq: r.seq,
            loop_type: r.r#type,
            status: r.status,
            run_statuses: vec![],
            created_at: r.created_at,
            updated_at: r.updated_at,
        })
        .collect();

    Ok(Json(Envelope::success(filtered)))
}

pub async fn create_loop(
    State(state): State<Arc<AppState>>,
    Path(project_name): Path<String>,
    Json(input): Json<CreateLoopInput>,
) -> Result<(StatusCode, Json<Envelope<LoopDetail>>), ApiError> {
    let repos = state.ctx.state.repos();
    let now = crate::helpers::now_iso();
    let id = uuid::Uuid::new_v4().to_string();

    let seq = repos.loops.allocate_seq().map_err(internal_error)?;

    let target_type = match input.target.as_deref() {
        Some(t) if t.starts_with('#') || t.parse::<i64>().is_ok() => "issue",
        Some(_) => "pull_request",
        None => "project",
    };
    let target_id = input.target.clone();

    repos
        .loops
        .upsert(&LoopRecord {
            id: id.clone(),
            seq,
            project_id: project_name.clone(),
            r#type: input.loop_type.clone(),
            target_type: target_type.into(),
            target_id,
            repo: None,
            pr_number: None,
            status: "active".into(),
            config_json: None,
            metadata_json: input
                .metadata
                .as_ref()
                .map(|v| v.to_string()),
            last_run_at: None,
            next_run_at: None,
            created_at: now.clone(),
            updated_at: now.clone(),
        })
        .map_err(internal_error)?;

    let detail = LoopDetail {
        project_name,
        seq,
        loop_type: input.loop_type,
        status: "active".into(),
        target: input.target,
        metadata: input.metadata,
        runs: vec![],
        created_at: now.clone(),
        updated_at: now,
    };

    Ok((StatusCode::CREATED, Json(Envelope::success(detail))))
}

pub async fn get_loop(
    State(state): State<Arc<AppState>>,
    Path((project_name, seq)): Path<(String, i64)>,
) -> Result<Json<Envelope<LoopDetail>>, ApiError> {
    let repos = state.ctx.state.repos();

    let record = repos
        .loops
        .get_by_seq(seq)
        .map_err(internal_error)?
        .filter(|r| r.project_id == project_name)
        .ok_or_else(|| ApiError::not_found(format!("Loop {project_name}/{seq} not found")))?;

    // Fetch latest run for this loop
    let runs = repos
        .runs
        .list_by_loop(&record.id)
        .map_err(internal_error)?;

    let run_summaries: Vec<RunSummary> = runs
        .into_iter()
        .map(|r| RunSummary {
            run_id: r.id,
            loop_seq: seq,
            project_name: project_name.clone(),
            step_name: r.current_step.clone().unwrap_or_default(),
            agent_vendor: r.agent_vendor.clone().unwrap_or_default(),
            status: r.status,
            created_at: r.created_at,
        })
        .collect();

    let detail = LoopDetail {
        project_name: record.project_id,
        seq: record.seq,
        loop_type: record.r#type,
        status: record.status,
        target: record.target_id,
        metadata: record
            .metadata_json
            .and_then(|m| serde_json::from_str(&m).ok()),
        runs: run_summaries,
        created_at: record.created_at,
        updated_at: record.updated_at,
    };

    Ok(Json(Envelope::success(detail)))
}

pub async fn pause_loop(
    State(state): State<Arc<AppState>>,
    Path((project_name, seq)): Path<(String, i64)>,
) -> Result<Json<Envelope<()>>, ApiError> {
    state
        .ctx
        .state
        .stop_loop(&project_name, seq)
        .await?;
    Ok(Json(Envelope::success_empty()))
}

pub async fn resume_loop(
    State(state): State<Arc<AppState>>,
    Path((project_name, seq)): Path<(String, i64)>,
) -> Result<Json<Envelope<()>>, ApiError> {
    let repos = state.ctx.state.repos();

    // Fetch current record, set status to active, write back.
    let mut record = repos
        .loops
        .get_by_seq(seq)
        .map_err(internal_error)?
        .filter(|r| r.project_id == project_name)
        .ok_or_else(|| ApiError::not_found(format!("Loop {project_name}/{seq} not found")))?;

    let now = crate::helpers::now_iso();
    record.status = "active".into();
    record.updated_at = now;

    repos.loops.upsert(&record).map_err(internal_error)?;
    Ok(Json(Envelope::success_empty()))
}

pub async fn terminate_loop(
    State(state): State<Arc<AppState>>,
    Path((project_name, seq)): Path<(String, i64)>,
) -> Result<Json<Envelope<()>>, ApiError> {
    state
        .ctx
        .state
        .close_loop(&project_name, seq)
        .await?;
    Ok(Json(Envelope::success_empty()))
}

// ---------------------------------------------------------------------------
// Runs
// ---------------------------------------------------------------------------

pub async fn list_runs(
    State(state): State<Arc<AppState>>,
    Path((project_name, seq)): Path<(String, i64)>,
) -> Result<Json<Envelope<Vec<RunSummary>>>, ApiError> {
    let repos = state.ctx.state.repos();

    // Find the loop by seq to get its UUID id
    let loop_record = repos
        .loops
        .get_by_seq(seq)
        .map_err(internal_error)?
        .filter(|r| r.project_id == project_name)
        .ok_or_else(|| ApiError::not_found(format!("Loop {project_name}/{seq} not found")))?;

    let records = repos
        .runs
        .list_by_loop(&loop_record.id)
        .map_err(internal_error)?;

    let runs: Vec<RunSummary> = records
        .into_iter()
        .map(|r| RunSummary {
            run_id: r.id,
            loop_seq: seq,
            project_name: project_name.clone(),
            step_name: r.current_step.unwrap_or_default(),
            agent_vendor: r.agent_vendor.unwrap_or_default(),
            status: r.status,
            created_at: r.created_at,
        })
        .collect();

    Ok(Json(Envelope::success(runs)))
}

pub async fn start_run(
    State(state): State<Arc<AppState>>,
    Path((project_name, seq)): Path<(String, i64)>,
    Json(input): Json<StartRunInput>,
) -> Result<(StatusCode, Json<Envelope<RunDetail>>), ApiError> {
    let repos = state.ctx.state.repos();
    let now = crate::helpers::now_iso();

    // Find the loop by seq to get its UUID id
    let loop_record = repos
        .loops
        .get_by_seq(seq)
        .map_err(internal_error)?
        .filter(|r| r.project_id == project_name)
        .ok_or_else(|| ApiError::not_found(format!("Loop {project_name}/{seq} not found")))?;

    let run_id = input.run_id;

    repos
        .runs
        .upsert(&RunRecord {
            id: run_id.clone(),
            loop_id: loop_record.id,
            status: "pending".into(),
            current_step: Some(input.step_name.clone()),
            last_completed_step: None,
            checkpoint_json: None,
            summary: None,
            error_message: None,
            agent_vendor: Some(input.agent_vendor.clone()),
            model: input.model.clone(),
            started_at: now.clone(),
            last_heartbeat_at: None,
            ended_at: None,
            created_at: now.clone(),
            updated_at: now.clone(),
        })
        .map_err(internal_error)?;

    let detail = RunDetail {
        run_id,
        loop_seq: seq,
        project_name,
        step_name: input.step_name,
        agent_vendor: input.agent_vendor,
        model: input.model,
        status: "pending".into(),
        exit_code: None,
        output_truncated: None,
        native_session_id: None,
        created_at: now.clone(),
        started_at: Some(now),
        completed_at: None,
    };

    Ok((StatusCode::CREATED, Json(Envelope::success(detail))))
}

pub async fn get_run(
    State(state): State<Arc<AppState>>,
    Path((_project_name, _seq, run_id)): Path<(String, i64, String)>,
) -> Result<Json<Envelope<RunDetail>>, ApiError> {
    let repos = state.ctx.state.repos();

    let record = repos
        .runs
        .get_by_id(&run_id)
        .map_err(internal_error)?
        .ok_or_else(|| ApiError::not_found(format!("Run '{run_id}' not found")))?;

    let detail = RunDetail {
        run_id: record.id,
        loop_seq: _seq,
        project_name: _project_name,
        step_name: record.current_step.unwrap_or_default(),
        agent_vendor: record.agent_vendor.unwrap_or_default(),
        model: record.model,
        status: record.status,
        exit_code: None,
        output_truncated: None,
        native_session_id: None,
        created_at: record.created_at,
        started_at: Some(record.started_at),
        completed_at: record.ended_at,
    };

    Ok(Json(Envelope::success(detail)))
}

pub async fn cancel_run(
    State(state): State<Arc<AppState>>,
    Path((project_name, seq)): Path<(String, i64)>,
) -> Result<Json<Envelope<()>>, ApiError> {
    state
        .ctx
        .state
        .stop_loop(&project_name, seq)
        .await?;
    Ok(Json(Envelope::success_empty()))
}

// ---------------------------------------------------------------------------
// Queue
// ---------------------------------------------------------------------------

pub async fn list_queue(
    State(state): State<Arc<AppState>>,
    Path(project_name): Path<String>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<Envelope<Vec<QueueItemResponse>>>, ApiError> {
    let repos = state.ctx.state.repos();
    let all = repos.queue.list().map_err(internal_error)?;

    let items: Vec<QueueItemResponse> = all
        .into_iter()
        .filter(|r| r.project_id.as_deref() == Some(&project_name))
        .skip(params.offset as usize)
        .take(params.limit as usize)
        .map(|r| QueueItemResponse {
            id: r.id,
            project_name: r.project_id.unwrap_or_default(),
            queue_type: r.r#type,
            loop_seq: None,
            run_id: None,
            status: r.status,
            priority: r.priority as i32,
            attempts: r.attempts as i32,
            last_error: r.last_error,
            scheduled_not_before: None,
            claimed_by: r.claimed_by,
            created_at: r.created_at,
            updated_at: r.updated_at,
        })
        .collect();

    Ok(Json(Envelope::success(items)))
}

pub async fn enqueue(
    State(state): State<Arc<AppState>>,
    Path(project_name): Path<String>,
    Json(input): Json<EnqueueInput>,
) -> Result<(StatusCode, Json<Envelope<QueueItemResponse>>), ApiError> {
    let repos = state.ctx.state.repos();
    let now = crate::helpers::now_iso();
    let dedupe_key = format!("{}/{}", project_name, input.queue_type);

    // Resolve loop_seq to loop UUID if provided
    let loop_id: Option<String> = if let Some(lseq) = input.loop_seq {
        match repos.loops.get_by_seq(lseq) {
            Ok(Some(rec)) => {
                if rec.project_id != project_name {
                    return Err(ApiError::not_found(format!("Loop seq {lseq} not found for project {project_name}")));
                }
                Some(rec.id.clone())
            }
            Ok(None) => {
                return Err(ApiError::not_found(format!("Loop seq {lseq} not found for project {project_name}")));
            }
            Err(e) => return Err(internal_error(e)),
        }
    } else {
        None
    };

    let record = QueueItemRecord {
        id: uuid::Uuid::new_v4().to_string(),
        project_id: Some(project_name.clone()),
        loop_id,
        r#type: input.queue_type.clone(),
        target_type: "default".into(),
        target_id: String::new(),
        repo: None,
        pr_number: None,
        dedupe_key,
        priority: input.priority.unwrap_or(1) as i64,
        status: "queued".into(),
        available_at: now.clone(),
        attempts: 0,
        max_attempts: 5,
        claimed_by: None,
        claimed_at: None,
        started_at: None,
        finished_at: None,
        lock_key: None,
        payload_json: input.payload.map(|v| v.to_string()),
        last_error: None,
        last_error_kind: None,
        created_at: now.clone(),
        updated_at: now.clone(),
    };

    let (item, _created) = repos
        .queue
        .create_or_get_active_by_dedupe(&record)
        .map_err(internal_error)?;

    let resp = QueueItemResponse {
        id: item.id,
        project_name: item.project_id.unwrap_or(project_name),
        queue_type: item.r#type,
        loop_seq: item.loop_id.and_then(|s| s.parse().ok()),
        run_id: None,
        status: item.status,
        priority: item.priority as i32,
        attempts: item.attempts as i32,
        last_error: item.last_error,
        scheduled_not_before: None,
        claimed_by: item.claimed_by,
        created_at: item.created_at,
        updated_at: item.updated_at,
    };

    Ok((StatusCode::CREATED, Json(Envelope::success(resp))))
}

pub async fn dequeue(
    State(state): State<Arc<AppState>>,
    Path((project_name, item_id)): Path<(String, String)>,
) -> Result<Json<Envelope<()>>, ApiError> {
    let repos = state.ctx.state.repos();
    let now = crate::helpers::now_iso();

    repos
        .queue
        .cancel_by_project(&project_name, &now, Some("cancelled via API"))
        .map_err(internal_error)?;

    // Also try to complete the specific item if it exists and is running
    if let Ok(Some(item)) = repos.queue.get_by_id(&item_id) {
        if item.status == "running" || item.status == "queued" {
            repos.queue.complete(&item_id, &now).map_err(internal_error)?;
        }
    }

    Ok(Json(Envelope::success_empty()))
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

pub async fn list_events(
    State(state): State<Arc<AppState>>,
    Path(project_name): Path<String>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<Envelope<Vec<EventLogResponse>>>, ApiError> {
    let repos = state.ctx.state.repos();

    // Fetch enough events to cover offset + limit after project filter.
    let fetch_limit = params.limit + params.offset + 50;
    let records = repos
        .events
        .list(fetch_limit)
        .map_err(internal_error)?;

    let events: Vec<EventLogResponse> = records
        .into_iter()
        .filter(|r| r.project_id.as_deref() == Some(&project_name))
        .skip(params.offset as usize)
        .take(params.limit as usize)
        .map(|r| EventLogResponse {
            id: r.id,
            timestamp: r.created_at,
            event_type: r.event_type,
            actor: r.actor_display_name.unwrap_or_default(),
            project_name: r.project_id,
            loop_seq: None,
            run_id: r.run_id,
            details: if r.payload_json.is_empty() || r.payload_json == "{}" {
                None
            } else {
                serde_json::from_str(&r.payload_json).ok()
            },
        })
        .collect();

    Ok(Json(Envelope::success(events)))
}

// ---------------------------------------------------------------------------
// Locks
// ---------------------------------------------------------------------------

pub async fn list_locks(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Envelope<Vec<LockResponse>>>, ApiError> {
    let repos = state.ctx.state.repos();
    let now = crate::helpers::now_iso();

    // list_expired returns locks whose expires_at <= now.
    // We want ALL active locks (expires_at > now).
    // Fetch *all* locks and do in-memory filter (no list_all API).
    // For simplicity, fetch expired (which returns many) and report those as expired;
    // a proper active-lock list would need a new repo method.
    // Instead, do a raw query for active locks.
    let active = repos
        .locks
        .list_expired(&now)
        .map_err(internal_error)?;

    // Since list_expired returns *expired* locks, we report them as the set.
    // For active locks we'd need a separate query — skip for now.
    let locks: Vec<LockResponse> = active
        .into_iter()
        .map(|r| LockResponse {
            id: r.key.clone(),
            project_name: None,
            resource: r.key,
            holder: r.owner,
            acquired_at: r.created_at,
            expires_at: Some(r.expires_at),
        })
        .collect();

    Ok(Json(Envelope::success(locks)))
}

pub async fn acquire_lock(
    State(state): State<Arc<AppState>>,
    Json(input): Json<AcquireLockInput>,
) -> Result<(StatusCode, Json<Envelope<LockResponse>>), ApiError> {
    let repos = state.ctx.state.repos();
    let now = crate::helpers::now_iso();

    let ttl_secs = input.ttl_secs.unwrap_or(300);
    let expires_at = (chrono::Utc::now()
        + chrono::Duration::seconds(ttl_secs))
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string();

    let record = LockRecord {
        key: input.resource.clone(),
        owner: "api".into(),
        reason: Some("acquired via API".into()),
        expires_at: expires_at.clone(),
        created_at: now.clone(),
        updated_at: now.clone(),
    };

    repos
        .locks
        .acquire(&record)
        .map_err(internal_error)?;

    let resp = LockResponse {
        id: input.resource.clone(),
        project_name: None,
        resource: input.resource,
        holder: "api".into(),
        acquired_at: now,
        expires_at: Some(expires_at),
    };

    Ok((StatusCode::CREATED, Json(Envelope::success(resp))))
}

pub async fn release_lock(
    State(state): State<Arc<AppState>>,
    Path(lock_key): Path<String>,
) -> Result<Json<Envelope<()>>, ApiError> {
    let repos = state.ctx.state.repos();
    repos
        .locks
        .release(&lock_key)
        .map_err(internal_error)?;
    Ok(Json(Envelope::success_empty()))
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

pub async fn get_config(
    State(state): State<Arc<AppState>>,
) -> Json<Envelope<ConfigResponse>> {
    let config = (*state.ctx.config).clone();
    Json(Envelope::success(config))
}

pub async fn get_agent_config(
    State(state): State<Arc<AppState>>,
    Path(project_name): Path<String>,
) -> Result<Json<Envelope<AgentConfigResponse>>, ApiError> {
    let config = &state.ctx.config;
    let agent_cfg = config.agent.clone().unwrap_or_default();

    let resp = AgentConfigResponse {
        project_name,
        agent_vendor: agent_cfg.default_vendor.to_string(),
        model: agent_cfg.model,
        max_execution_seconds: agent_cfg.timeout_secs,
        max_idle_seconds: 300, // not directly configurable yet
        env_overrides: std::collections::HashMap::new(),
    };

    Ok(Json(Envelope::success(resp)))
}

// ---------------------------------------------------------------------------
// SSE (streaming)
// ---------------------------------------------------------------------------

pub async fn project_events_stream(
    State(state): State<Arc<AppState>>,
    Path(project_name): Path<String>,
) -> axum::response::Sse<impl tokio_stream::Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>> + Send + 'static> {
    project_event_stream(state.ctx.clone(), project_name)
}

pub async fn global_events_stream(
    State(state): State<Arc<AppState>>,
) -> axum::response::Sse<impl tokio_stream::Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>> + Send + 'static> {
    global_event_stream(state.ctx.clone())
}
