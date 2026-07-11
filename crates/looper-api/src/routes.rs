use std::sync::Arc;
use std::time::Instant;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::envelope::Envelope;
use crate::error::ApiError;
use crate::sse::{global_event_stream, project_event_stream};
use crate::types::{
    internal_error, AcquireLockInput, AddProjectInput, AdmitWorkRequest, AdmitWorkResponse, AgentConfigResponse,
    ConfigResponse, CreateLoopInput, EnqueueInput, EventLogResponse, HealthResponse, LockResponse, LoopDetail,
    LoopSummary, PaginationParams, ProjectSummary, QueueItemResponse, RunDetail, RunSummary, StartRunInput,
    VersionResponse,
};
use looper_storage::record::{LockRecord, LoopRecord, QueueItemRecord, RunRecord};

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
    let resp = VersionResponse { version: env!("CARGO_PKG_VERSION").to_string() };
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

pub async fn update_project(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(input): Json<crate::types::UpdateProjectInput>,
) -> Result<Json<Envelope<ProjectSummary>>, ApiError> {
    let updated = state.ctx.projects.update(&name, input).await?;
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
            metadata_json: input.metadata.as_ref().map(|v| v.to_string()),
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
        worktree_path: None,
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
    let runs = repos.runs.list_by_loop(&record.id).map_err(internal_error)?;

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

    // Resolve worktree path from the worktrees table (planner/reviewer store loop_id
    // in metadata and/or branch), falling back to loop metadata.worktree_path.
    let worktree_path =
        repos.worktrees.get_latest_by_loop_id(&record.id).map_err(internal_error)?.map(|wt| wt.worktree_path).or_else(
            || {
                record
                    .metadata_json
                    .as_deref()
                    .and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok())
                    .and_then(|v| v.get("worktree_path").and_then(|p| p.as_str().map(String::from)))
            },
        );

    let detail = LoopDetail {
        project_name: record.project_id,
        seq: record.seq,
        loop_type: record.r#type,
        status: record.status,
        target: record.target_id,
        metadata: record.metadata_json.and_then(|m| serde_json::from_str(&m).ok()),
        worktree_path,
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
    // Pause is reversible — do NOT call stop_loop (terminal).
    let repos = state.ctx.state.repos();
    let record = repos
        .loops
        .get_by_seq(seq)
        .map_err(internal_error)?
        .filter(|r| r.project_id == project_name)
        .ok_or_else(|| ApiError::not_found(format!("Loop {project_name}/{seq} not found")))?;

    let now = crate::helpers::now_iso();
    let from = record.status.clone();
    if record.status == "stopped" || record.status == "closed" || record.status == "terminated" {
        return Err(ApiError::bad_request(format!(
            "cannot pause terminal loop {project_name}/{seq} (status={})",
            record.status
        )));
    }
    // UPDATE only — never REPLACE (would CASCADE-delete queue items).
    repos.loops.update_status(&record.id, "paused", &now).map_err(internal_error)?;

    // Hold work: cancel active queue items for this loop (resume requeues).
    let cancelled = repos.queue.cancel_by_loop(&record.id, &now, Some("paused by user")).map_err(internal_error)?;

    info!(
        project = %project_name,
        seq,
        loop_id = %record.id,
        from_status = %from,
        to_status = "paused",
        queue_cancelled = cancelled,
        "loop paused"
    );
    Ok(Json(Envelope::success_empty()))
}

pub async fn resume_loop(
    State(state): State<Arc<AppState>>,
    Path((project_name, seq)): Path<(String, i64)>,
) -> Result<Json<Envelope<()>>, ApiError> {
    let repos = state.ctx.state.repos();

    let record = repos
        .loops
        .get_by_seq(seq)
        .map_err(internal_error)?
        .filter(|r| r.project_id == project_name)
        .ok_or_else(|| ApiError::not_found(format!("Loop {project_name}/{seq} not found")))?;

    if record.status == "stopped" || record.status == "closed" || record.status == "terminated" {
        return Err(ApiError::bad_request(format!(
            "cannot resume terminal loop {project_name}/{seq} (status={})",
            record.status
        )));
    }

    let now = crate::helpers::now_iso();
    let from = record.status.clone();
    repos.loops.update_status(&record.id, "active", &now).map_err(internal_error)?;

    // Re-activate most recent cancelled/failed queue item for this loop.
    let mut requeued = repos.queue.requeue_latest_cancelled_by_loop(&record.id, &now).map_err(internal_error)?;
    if requeued == 0 {
        requeued = repos.queue.requeue_latest_failed_by_loop(&record.id, &now).map_err(internal_error)?;
    }
    if requeued == 0 {
        // Also try requeue running stuck items.
        requeued = repos.queue.requeue_running_by_loop(&record.id, &now).map_err(internal_error)?;
    }

    state.ctx.state.trigger_scheduler_tick().await;

    info!(
        project = %project_name,
        seq,
        loop_id = %record.id,
        from_status = %from,
        to_status = "active",
        queue_requeued = requeued,
        tick_triggered = true,
        "loop resumed"
    );
    Ok(Json(Envelope::success_empty()))
}

pub async fn terminate_loop(
    State(state): State<Arc<AppState>>,
    Path((project_name, seq)): Path<(String, i64)>,
) -> Result<Json<Envelope<()>>, ApiError> {
    state.ctx.state.close_loop(&project_name, seq).await?;
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

    let records = repos.runs.list_by_loop(&loop_record.id).map_err(internal_error)?;

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
    state.ctx.state.stop_loop(&project_name, seq).await?;
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

    let (item, _created) = repos.queue.create_or_get_active_by_dedupe(&record).map_err(internal_error)?;

    // Immediate scheduling so manual enqueue is not stuck until poll_interval.
    state.ctx.state.trigger_scheduler_tick().await;

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

// ---------------------------------------------------------------------------
// Admit work (B2)
// ---------------------------------------------------------------------------

fn map_service_error(e: looper_service::ServiceError) -> ApiError {
    use looper_service::ServiceError;
    match e {
        ServiceError::ProjectNotFound(id) => ApiError::not_found(format!("project '{id}' not found")),
        ServiceError::LoopNotFound(id) => ApiError::not_found(format!("loop '{id}' not found")),
        ServiceError::RunNotFound(id) => ApiError::not_found(format!("run '{id}' not found")),
        ServiceError::ActiveLoopConflict { project_id, loop_type, target_key } => {
            ApiError::conflict(format!("active loop conflict {project_id}/{loop_type}/{target_key}"))
        }
        ServiceError::LoopHasRunningRun { loop_id } => {
            ApiError::conflict(format!("loop '{loop_id}' already has a running run"))
        }
        ServiceError::ProjectRepoUnresolved(msg) => ApiError::bad_request(msg),
        ServiceError::InvalidProjectID(msg) | ServiceError::Other(msg) => ApiError::bad_request(msg),
        ServiceError::Domain(d) => ApiError::bad_request(d.to_string()),
        other => ApiError::internal(other.to_string()),
    }
}

pub async fn admit_work(
    State(state): State<Arc<AppState>>,
    Path(project_name): Path<String>,
    Json(input): Json<AdmitWorkRequest>,
) -> Result<(StatusCode, Json<Envelope<AdmitWorkResponse>>), ApiError> {
    // All SQLite / service work is synchronous and must complete before `.await`
    // so the handler future stays `Send` (rusqlite Connection is !Send).
    let response = {
        let repos = state.ctx.state.repos_arc();

        let project_id = {
            let list = repos.projects.list().map_err(internal_error)?;
            list.into_iter()
                .find(|p| p.id == project_name || p.name == project_name)
                .map(|p| p.id)
                .ok_or_else(|| ApiError::not_found(format!("project '{project_name}' not found")))?
        };

        let svc = looper_service::AdmitWorkService::new(Arc::clone(&repos), chrono::Utc::now);
        let result = svc
            .admit_work(looper_service::AdmitWorkInput {
                project_id: project_id.clone(),
                role: input.role.clone(),
                issue_number: input.issue_number,
                pr_number: input.pr_number,
                repo: input.repo,
                priority: input.priority,
                metadata: input.metadata,
            })
            .map_err(map_service_error)?;

        let loop_rec = result.loop_record;
        let item = result.queue_item;

        info!(
            project = %project_name,
            role = %input.role,
            loop_id = %loop_rec.id,
            queue_id = %item.id,
            created_new_loop = result.created_new_loop,
            tick_triggered = true,
            "admit_work"
        );

        AdmitWorkResponse {
            loop_detail: LoopDetail {
                project_name: loop_rec.project_id.clone(),
                seq: loop_rec.seq,
                loop_type: loop_rec.r#type.clone(),
                status: loop_rec.status.clone(),
                target: loop_rec.target_id.clone(),
                metadata: loop_rec.metadata_json.as_deref().and_then(|s| serde_json::from_str(s).ok()),
                worktree_path: None,
                runs: vec![],
                created_at: loop_rec.created_at.clone(),
                updated_at: loop_rec.updated_at.clone(),
            },
            queue_item: QueueItemResponse {
                id: item.id.clone(),
                project_name: item.project_id.clone().unwrap_or(project_id),
                queue_type: item.r#type.clone(),
                loop_seq: Some(loop_rec.seq),
                run_id: None,
                status: item.status.clone(),
                priority: item.priority as i32,
                attempts: item.attempts as i32,
                last_error: item.last_error.clone(),
                scheduled_not_before: None,
                claimed_by: item.claimed_by.clone(),
                created_at: item.created_at.clone(),
                updated_at: item.updated_at.clone(),
            },
            created_new_loop: result.created_new_loop,
            tick_triggered: true,
        }
    };

    state.ctx.state.trigger_scheduler_tick().await;

    Ok((StatusCode::CREATED, Json(Envelope::success(response))))
}

pub async fn dequeue(
    State(state): State<Arc<AppState>>,
    Path((project_name, item_id)): Path<(String, String)>,
) -> Result<Json<Envelope<()>>, ApiError> {
    let repos = state.ctx.state.repos();
    let now = crate::helpers::now_iso();

    // Single-item mutation only: cancel the target item_id for this project.
    let item = repos.queue.get_by_id(&item_id).map_err(internal_error)?;
    let Some(item) = item else {
        return Err(ApiError::not_found(format!("queue item {item_id} not found")));
    };
    if item.project_id.as_deref() != Some(project_name.as_str()) {
        return Err(ApiError::not_found(format!("queue item {item_id} not found for project {project_name}")));
    }

    if item.status == "queued" || item.status == "running" {
        repos.queue.cancel_by_id(&item_id, &now, Some("cancelled via API")).map_err(internal_error)?;
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
    let records = repos.events.list(fetch_limit).map_err(internal_error)?;

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

pub async fn list_locks(State(state): State<Arc<AppState>>) -> Result<Json<Envelope<Vec<LockResponse>>>, ApiError> {
    let repos = state.ctx.state.repos();
    let now = crate::helpers::now_iso();

    // Active inventory: expires_at > now (not the expired set).
    let active = repos.locks.list_active(&now).map_err(internal_error)?;

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
    let expires_at =
        (chrono::Utc::now() + chrono::Duration::seconds(ttl_secs)).format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();

    let record = LockRecord {
        key: input.resource.clone(),
        owner: "api".into(),
        reason: Some("acquired via API".into()),
        expires_at: expires_at.clone(),
        created_at: now.clone(),
        updated_at: now.clone(),
    };

    repos.locks.acquire(&record).map_err(internal_error)?;

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
    repos.locks.release(&lock_key).map_err(internal_error)?;
    Ok(Json(Envelope::success_empty()))
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

pub async fn get_config(State(state): State<Arc<AppState>>) -> Json<Envelope<ConfigResponse>> {
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
) -> axum::response::Sse<
    impl tokio_stream::Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>> + Send + 'static,
> {
    project_event_stream(state.ctx.clone(), project_name)
}

pub async fn global_events_stream(
    State(state): State<Arc<AppState>>,
) -> axum::response::Sse<
    impl tokio_stream::Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>> + Send + 'static,
> {
    global_event_stream(state.ctx.clone())
}

// ---------------------------------------------------------------------------
// Worktree cleanup
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct WorktreeCleanupInput {
    pub include_orphans: Option<bool>,
    pub retention_days: Option<u64>,
    pub max_per_tick: Option<usize>,
    pub dry_run: Option<bool>,
    pub project_id: Option<String>,
}

#[derive(Serialize)]
pub struct WorktreeCleanupResult {
    pub scanned: usize,
    pub cleaned: usize,
    pub errors: Vec<String>,
    pub summary: serde_json::Value,
}

pub async fn worktree_cleanup(
    State(state): State<Arc<AppState>>,
    Json(input): Json<WorktreeCleanupInput>,
) -> Result<Json<Envelope<WorktreeCleanupResult>>, ApiError> {
    let repos = state.ctx.state.repos_arc();

    let options = looper_infra::worktree_cleanup::CleanupOptions {
        include_orphans: input.include_orphans.unwrap_or(true),
        retention_days: input.retention_days.unwrap_or(7),
        max_per_tick: input.max_per_tick.unwrap_or(10),
        dry_run: input.dry_run.unwrap_or(false),
        project_id: input.project_id,
    };

    match looper_infra::worktree_cleanup::run_cycle(&repos, &options) {
        Ok(run_result) => {
            let summary_json = serde_json::json!({
                "scanned": run_result.summary.scanned,
                "candidates": run_result.summary.candidates,
                "would_clean": run_result.summary.would_clean,
                "skipped": run_result.summary.skipped,
                "orphans": run_result.summary.orphans,
            });
            let result = WorktreeCleanupResult {
                scanned: run_result.summary.scanned,
                cleaned: run_result.cleaned_count,
                errors: run_result.errors,
                summary: summary_json,
            };
            Ok(Json(Envelope::success(result)))
        }
        Err(e) => Err(ApiError::internal(format!("worktree cleanup failed: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use looper_storage::migration::run_migrations;
    use looper_storage::repos::EventsRepository;
    use looper_storage::{EventLog, Repositories};
    use rusqlite::Connection;
    use std::sync::Arc;

    use crate::types::{Context, ProjectService, RuntimeState};

    struct TestState {
        repos: Arc<Repositories>,
        event_log: EventLog,
        ticks: Arc<std::sync::atomic::AtomicUsize>,
    }

    // rusqlite::Connection is !Send/!Sync; tests are single-threaded.
    unsafe impl Send for TestState {}
    unsafe impl Sync for TestState {}

    #[async_trait]
    impl RuntimeState for TestState {
        fn repos(&self) -> &Repositories {
            &self.repos
        }

        fn repos_arc(&self) -> Arc<Repositories> {
            Arc::clone(&self.repos)
        }

        fn event_log(&self) -> &EventLog {
            &self.event_log
        }

        async fn stop_loop(&self, _: &str, _: i64) -> Result<(), ApiError> {
            Ok(())
        }

        async fn close_loop(&self, _: &str, _: i64) -> Result<(), ApiError> {
            Ok(())
        }

        async fn stop_all(&self, _: &str) -> Result<(), ApiError> {
            Ok(())
        }

        async fn repair_reviewer(&self, _: &str) -> Result<(), ApiError> {
            Ok(())
        }

        async fn trigger_scheduler_tick(&self) {
            self.ticks.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    struct StubProjects {
        service: looper_service::ProjectService,
    }

    // ProjectService holds !Send/!Sync storage; tests are single-threaded.
    unsafe impl Send for StubProjects {}
    unsafe impl Sync for StubProjects {}

    #[async_trait]
    impl ProjectService for StubProjects {
        async fn add(&self, input: AddProjectInput) -> Result<ProjectSummary, ApiError> {
            let result = self
                .service
                .add_project(looper_service::AddInput {
                    id: input.name.clone(),
                    name: input.name,
                    repo_path: input.path.unwrap_or_default(),
                    base_branch: input.default_branch.unwrap_or_else(|| "main".into()),
                    id_source: "explicit".into(),
                    worktree_root: None,
                    repo: input.repo_url,
                    snapshot_mode: looper_service::SnapshotMode::Off,
                })
                .map_err(|e| ApiError::internal(e.to_string()))?;
            Ok(summary_from_record(result.project, result.repo))
        }

        async fn remove(&self, name: &str) -> Result<(), ApiError> {
            self.service.remove_project(name).map_err(|e| ApiError::internal(e.to_string()))?;
            Ok(())
        }

        async fn list(&self) -> Result<Vec<ProjectSummary>, ApiError> {
            let records = self.service.list().map_err(|e| ApiError::internal(e.to_string()))?;
            Ok(records.into_iter().map(|r| summary_from_record(r, None)).collect())
        }

        async fn sync(&self, _: &str) -> Result<ProjectSummary, ApiError> {
            Err(ApiError::internal("not used"))
        }

        async fn update(
            &self,
            name: &str,
            input: crate::types::UpdateProjectInput,
        ) -> Result<ProjectSummary, ApiError> {
            let rec = self
                .service
                .update_project(
                    name,
                    looper_service::UpdateInput {
                        schedule: input.schedule,
                        enabled: input.enabled,
                        archive_filter: input.archive_filter,
                        default_branch: input.default_branch,
                        path: input.path,
                        repo: input.repo_url,
                    },
                )
                .map_err(|e| match e {
                    looper_service::ServiceError::ProjectNotFound(id) => ApiError::not_found(id),
                    other => ApiError::internal(other.to_string()),
                })?;
            Ok(summary_from_record(rec, None))
        }
    }

    fn summary_from_record(rec: looper_storage::ProjectRecord, repo_url: Option<String>) -> ProjectSummary {
        let meta = rec.metadata_json.as_deref().and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok());
        let repo_url = repo_url.or_else(|| looper_service::effective_project_repo(&rec));
        let schedule = meta
            .as_ref()
            .and_then(|v| v.get("schedule").and_then(|s| s.as_str().map(String::from)))
            .unwrap_or_default();
        let archive_filter =
            meta.as_ref().and_then(|v| v.get("archive_filter").and_then(|s| s.as_str().map(String::from)));
        ProjectSummary {
            name: rec.id,
            path: rec.repo_path,
            repo_url,
            default_branch: rec.base_branch.unwrap_or_default(),
            schedule,
            enabled: !rec.archived,
            archive_filter,
        }
    }

    fn setup_state() -> (Arc<AppState>, Arc<std::sync::atomic::AtomicUsize>) {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        run_migrations(&mut conn).unwrap();
        let repos = Arc::new(Repositories::new(conn));

        let event_conn = Connection::open_in_memory().unwrap();
        let event_log = EventLog::new(EventsRepository::new(Arc::new(event_conn)));

        let project_svc = looper_service::ProjectService::new(
            Arc::clone(&repos),
            looper_service::ProjectServiceCallbacks::new(),
            chrono::Utc::now,
        );
        let ticks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let state = TestState { repos: Arc::clone(&repos), event_log, ticks: Arc::clone(&ticks) };
        let ctx = Arc::new(Context {
            config: Arc::new(looper_config::Config::default()),
            state: Arc::new(state),
            projects: Arc::new(StubProjects { service: project_svc }),
        });
        (Arc::new(AppState { ctx, started_at: Instant::now() }), ticks)
    }

    #[tokio::test]
    async fn dequeue_cancels_only_target_item() {
        let (state, ticks) = setup_state();
        let project = "proj-a".to_string();
        let now = crate::helpers::now_iso();
        state
            .ctx
            .state
            .repos()
            .projects
            .upsert(&looper_storage::ProjectRecord {
                id: project.clone(),
                name: project.clone(),
                repo_path: "/tmp/proj-a".into(),
                base_branch: Some("main".into()),
                archived: false,
                metadata_json: None,
                created_at: now.clone(),
                updated_at: now,
            })
            .unwrap();

        // Enqueue two items with different queue_types (dedupe key includes type).
        let (status_a, Json(env_a)) = enqueue(
            State(Arc::clone(&state)),
            Path(project.clone()),
            Json(EnqueueInput {
                queue_type: "reviewer".into(),
                loop_seq: None,
                run_id: None,
                priority: Some(1),
                payload: None,
            }),
        )
        .await
        .expect("enqueue A");
        assert_eq!(status_a, StatusCode::CREATED);
        let item_a = env_a.data.expect("item A data");
        let id_a = item_a.id.clone();

        let (status_b, Json(env_b)) = enqueue(
            State(Arc::clone(&state)),
            Path(project.clone()),
            Json(EnqueueInput {
                queue_type: "fixer".into(),
                loop_seq: None,
                run_id: None,
                priority: Some(1),
                payload: None,
            }),
        )
        .await
        .expect("enqueue B");
        assert_eq!(status_b, StatusCode::CREATED);
        let item_b = env_b.data.expect("item B data");
        let id_b = item_b.id.clone();

        eprintln!("dequeue test item_a={id_a} item_b={id_b}");

        // Dequeue only A
        let _ = dequeue(State(Arc::clone(&state)), Path((project.clone(), id_a.clone()))).await.expect("dequeue A");

        let repos = state.ctx.state.repos();
        let after_a = repos.queue.get_by_id(&id_a).unwrap().expect("A still exists");
        let after_b = repos.queue.get_by_id(&id_b).unwrap().expect("B still exists");

        assert_eq!(after_a.status, "cancelled", "A should be cancelled; got {}", after_a.status);
        assert_eq!(after_b.status, "queued", "B must remain queued; got {}", after_b.status);
        // Each enqueue triggers a tick.
        assert!(
            ticks.load(std::sync::atomic::Ordering::SeqCst) >= 2,
            "enqueue should trigger scheduler ticks, got {}",
            ticks.load(std::sync::atomic::Ordering::SeqCst)
        );
    }

    #[tokio::test]
    async fn dequeue_missing_item_returns_404() {
        let (state, _) = setup_state();
        let err =
            dequeue(State(state), Path(("proj-a".into(), "does-not-exist".into()))).await.expect_err("expected 404");
        assert_eq!(err.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn update_project_persists_and_get_reflects() {
        let (state, _) = setup_state();

        let (status, Json(env)) = add_project(
            State(Arc::clone(&state)),
            Json(AddProjectInput {
                name: "p-upd".into(),
                path: Some("/tmp/p-upd".into()),
                repo_url: Some("org/p-upd".into()),
                default_branch: Some("main".into()),
                schedule: None,
                enabled: true,
                archive_filter: None,
            }),
        )
        .await
        .expect("add");
        assert_eq!(status, StatusCode::CREATED);
        let _ = env.data.expect("created project");

        let Json(put_env) = update_project(
            State(Arc::clone(&state)),
            Path("p-upd".into()),
            Json(crate::types::UpdateProjectInput {
                schedule: Some("0 0 * * *".into()),
                enabled: Some(true),
                archive_filter: Some("closed".into()),
                default_branch: Some("develop".into()),
                path: Some("/tmp/p-upd-new".into()),
                repo_url: None,
            }),
        )
        .await
        .expect("update");
        let put = put_env.data.expect("put data");
        assert_eq!(put.default_branch, "develop");
        assert_eq!(put.path, "/tmp/p-upd-new");
        assert_eq!(put.schedule, "0 0 * * *");
        assert_eq!(put.archive_filter.as_deref(), Some("closed"));
        assert!(put.enabled);

        let Json(get_env) = get_project(State(Arc::clone(&state)), Path("p-upd".into())).await.expect("get");
        let got = get_env.data.expect("get data");
        assert_eq!(got.default_branch, "develop");
        assert_eq!(got.path, "/tmp/p-upd-new");
        assert_eq!(got.schedule, "0 0 * * *");
        assert_eq!(got.archive_filter.as_deref(), Some("closed"));
    }

    #[tokio::test]
    async fn admit_work_creates_queue_and_triggers_tick() {
        let (state, ticks) = setup_state();
        let now = crate::helpers::now_iso();
        state
            .ctx
            .state
            .repos()
            .projects
            .upsert(&looper_storage::ProjectRecord {
                id: "proj-work".into(),
                name: "proj-work".into(),
                repo_path: "/tmp/proj-work".into(),
                base_branch: Some("main".into()),
                archived: false,
                metadata_json: Some(r#"{"repo":"acme/widget"}"#.into()),
                created_at: now.clone(),
                updated_at: now,
            })
            .unwrap();

        let (status, Json(env)) = admit_work(
            State(Arc::clone(&state)),
            Path("proj-work".into()),
            Json(AdmitWorkRequest {
                role: "planner".into(),
                issue_number: Some(12),
                pr_number: None,
                repo: None,
                priority: None,
                metadata: None,
            }),
        )
        .await
        .expect("admit_work");

        assert_eq!(status, StatusCode::CREATED);
        let data = env.data.expect("data");
        assert_eq!(data.queue_item.queue_type, "planner");
        assert_eq!(data.queue_item.status, "queued");
        assert!(data.tick_triggered);
        assert_eq!(data.loop_detail.loop_type, "planner");
        assert!(ticks.load(std::sync::atomic::Ordering::SeqCst) >= 1, "tick should fire after admit_work");

        // Idempotent re-admit
        let before_ticks = ticks.load(std::sync::atomic::Ordering::SeqCst);
        let (_, Json(env2)) = admit_work(
            State(Arc::clone(&state)),
            Path("proj-work".into()),
            Json(AdmitWorkRequest {
                role: "planner".into(),
                issue_number: Some(12),
                pr_number: None,
                repo: None,
                priority: None,
                metadata: None,
            }),
        )
        .await
        .expect("re-admit");
        let data2 = env2.data.expect("data2");
        assert_eq!(data2.queue_item.id, data.queue_item.id, "dedupe same queue item");
        assert!(!data2.created_new_loop);
        assert!(ticks.load(std::sync::atomic::Ordering::SeqCst) > before_ticks);
    }

    #[tokio::test]
    async fn admit_work_missing_repo_is_400() {
        let (state, _) = setup_state();
        let now = crate::helpers::now_iso();
        state
            .ctx
            .state
            .repos()
            .projects
            .upsert(&looper_storage::ProjectRecord {
                id: "no-repo".into(),
                name: "no-repo".into(),
                repo_path: "/tmp/no-repo".into(),
                base_branch: Some("main".into()),
                archived: false,
                metadata_json: None,
                created_at: now.clone(),
                updated_at: now,
            })
            .unwrap();

        let err = admit_work(
            State(state),
            Path("no-repo".into()),
            Json(AdmitWorkRequest {
                role: "planner".into(),
                issue_number: Some(1),
                pr_number: None,
                repo: None,
                priority: None,
                metadata: None,
            }),
        )
        .await
        .expect_err("expected bad request");
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn pause_is_reversible_resume_requeues() {
        let (state, ticks) = setup_state();
        let repos = state.ctx.state.repos();
        let now = crate::helpers::now_iso();
        repos
            .projects
            .upsert(&looper_storage::ProjectRecord {
                id: "p-pause".into(),
                name: "p-pause".into(),
                repo_path: "/tmp/p-pause".into(),
                base_branch: Some("main".into()),
                archived: false,
                metadata_json: Some(r#"{"repo":"acme/p"}"#.into()),
                created_at: now.clone(),
                updated_at: now.clone(),
            })
            .unwrap();

        let (_, Json(env)) = admit_work(
            State(Arc::clone(&state)),
            Path("p-pause".into()),
            Json(AdmitWorkRequest {
                role: "worker".into(),
                issue_number: Some(3),
                pr_number: None,
                repo: None,
                priority: None,
                metadata: None,
            }),
        )
        .await
        .expect("admit");
        let data = env.data.expect("data");
        let seq = data.loop_detail.seq;
        let qid = data.queue_item.id.clone();

        let _ = pause_loop(State(Arc::clone(&state)), Path(("p-pause".into(), seq))).await.expect("pause");
        let loop_rec = repos.loops.get_by_seq(seq).unwrap().expect("loop after pause");
        assert_eq!(loop_rec.status, "paused");
        let q = repos
            .queue
            .get_by_id(&qid)
            .unwrap()
            .or_else(|| repos.queue.get_latest_by_loop_id(&loop_rec.id).unwrap())
            .expect("queue item after pause");
        assert_eq!(q.status, "cancelled", "pause should cancel active queue; id={}", q.id);

        let before = ticks.load(std::sync::atomic::Ordering::SeqCst);
        let _ = resume_loop(State(Arc::clone(&state)), Path(("p-pause".into(), seq))).await.expect("resume");
        let loop_rec = repos.loops.get_by_seq(seq).unwrap().expect("loop after resume");
        assert_eq!(loop_rec.status, "active");
        let q = repos
            .queue
            .get_by_id(&qid)
            .unwrap()
            .or_else(|| repos.queue.get_latest_by_loop_id(&loop_rec.id).unwrap())
            .expect("queue item after resume");
        assert_eq!(q.status, "queued", "resume should requeue cancelled item");
        assert!(ticks.load(std::sync::atomic::Ordering::SeqCst) > before);
    }

    #[tokio::test]
    async fn get_loop_includes_worktree_path_from_table() {
        let (state, _) = setup_state();
        let repos = state.ctx.state.repos();
        let now = crate::helpers::now_iso();

        repos
            .projects
            .upsert(&looper_storage::ProjectRecord {
                id: "proj-wt".into(),
                name: "proj-wt".into(),
                repo_path: "/tmp/proj-wt".into(),
                base_branch: Some("main".into()),
                archived: false,
                metadata_json: None,
                created_at: now.clone(),
                updated_at: now.clone(),
            })
            .unwrap();

        let loop_id = "loop-wt-1".to_string();
        repos
            .loops
            .upsert(&LoopRecord {
                id: loop_id.clone(),
                seq: 7,
                project_id: "proj-wt".into(),
                r#type: "planner".into(),
                target_type: "issue".into(),
                target_id: Some("1".into()),
                repo: None,
                pr_number: None,
                status: "active".into(),
                config_json: None,
                metadata_json: None,
                last_run_at: None,
                next_run_at: None,
                created_at: now.clone(),
                updated_at: now.clone(),
            })
            .unwrap();

        repos
            .worktrees
            .upsert(&looper_storage::WorktreeRecord {
                id: "wt-1".into(),
                project_id: "proj-wt".into(),
                repo_path: "/tmp/proj-wt".into(),
                worktree_path: "/tmp/proj-wt/.looper/worktrees/planner-loop".into(),
                branch: format!("planner/{loop_id}"),
                base_branch: None,
                status: "created".into(),
                head_sha: None,
                metadata_json: Some(format!(r#"{{"loop_id":"{loop_id}"}}"#)),
                created_at: now.clone(),
                updated_at: now,
                cleaned_at: None,
            })
            .unwrap();

        let Json(env) = get_loop(State(state), Path(("proj-wt".into(), 7))).await.expect("get_loop");
        let detail = env.data.expect("loop detail");
        assert_eq!(detail.worktree_path.as_deref(), Some("/tmp/proj-wt/.looper/worktrees/planner-loop"));
    }
}
