pub mod archive;
pub mod diary;
pub mod dispatch;
pub mod inspect_view;
pub mod lesson_ingest;
pub mod reactions;
pub mod scoring;
pub mod status_view;

use anyhow::{Context, Result};
use camino::Utf8Path;
use grove_br::{BrClient, BrDependencySnapshot};
use grove_bv::BvTriageOutput;
use grove_config::{
    DEFAULT_CHECKPOINTS_DIR_NAME, DEFAULT_GROVE_DIR_NAME, DEFAULT_LOGS_DIR_NAME, GroveConfig,
};
use grove_db::{
    Database, HandoffWriteInput, InterruptedRunRecovery, LeaderLeaseAcquireInput,
    RecoveredReservation, RecoveryCapsuleWriteInput, ReservationAcquireOutcome, ReservationRequest,
    RunFinishInput, RunStartInput, SessionCheckpointInput,
};
use grove_session::{
    ClaudeBackend, SessionLifecycleHooks, SingleTaskSessionRequest, SingleTaskSessionResult,
    execute_single_task_session_with_hooks, update_circuit_breaker,
};
use grove_types::{
    AgentActivity, BeadId, CheckpointId, CircuitState, FailureClass, GroveBeadRecord,
    GroveBeadStatus, ProgressSignal, ReservationConflict, ReservationMode, ReservationRecord,
    RunId, RunStatus, SessionStatus, Timestamp,
};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

pub use dispatch::{
    BlockedReasonCount, BlockedSampleBead, BlockedSampleReason, DispatchBlockedSummary,
    DispatchExitReason, DispatchLoopConfig, DispatchLoopOutcome, ShutdownSignal, run_dispatch_loop,
};
pub use inspect_view::BeadInspectView;
pub use status_view::WorkspaceStatusView;

pub const CRATE_PURPOSE: &str = "Core Grove runtime domain and service boundaries.";

static TRACE_LOGGER: OnceLock<Mutex<Option<fs::File>>> = OnceLock::new();

#[derive(Debug, Clone)]
pub struct PersistedTaskRunOutcome {
    pub run: grove_types::TaskRunRecord,
    pub session: grove_types::SessionOutcome,
    pub checkpoint: Option<grove_types::CheckpointRecord>,
    pub handoff: Option<grove_types::HandoffRecord>,
}

#[derive(Debug, Clone)]
struct CheckpointFilePersistError {
    path: PathBuf,
    source: String,
}

impl std::fmt::Display for CheckpointFilePersistError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "failed to persist checkpoint file {}: {}",
            self.path.display(),
            self.source
        )
    }
}

impl std::error::Error for CheckpointFilePersistError {}

pub fn init_trace_logging(workspace_root: &Utf8Path, enabled: bool) -> Result<()> {
    let logger = TRACE_LOGGER.get_or_init(|| Mutex::new(None));
    let mut guard = logger
        .lock()
        .map_err(|_| anyhow::anyhow!("trace logger mutex poisoned"))?;

    if !enabled {
        *guard = None;
        return Ok(());
    }

    let logs_dir = workspace_root
        .join(DEFAULT_GROVE_DIR_NAME)
        .join(DEFAULT_LOGS_DIR_NAME);
    fs::create_dir_all(&logs_dir)
        .with_context(|| format!("create logs directory {}", logs_dir.as_str()))?;
    let log_path = logs_dir.join("runtime.jsonl");
    let file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path.as_std_path())
        .with_context(|| format!("open runtime trace log {}", log_path.as_str()))?;
    *guard = Some(file);
    Ok(())
}

fn write_trace_event(payload: serde_json::Value) {
    let Some(logger) = TRACE_LOGGER.get() else {
        return;
    };
    let Ok(mut guard) = logger.lock() else {
        return;
    };
    let Some(file) = guard.as_mut() else {
        return;
    };
    let mut line = payload;
    if let serde_json::Value::Object(ref mut object) = line {
        object.insert("ts".to_owned(), serde_json::json!(chrono::Utc::now()));
    }
    if let Ok(encoded) = serde_json::to_vec(&line) {
        let _ = file.write_all(&encoded);
        let _ = file.write_all(b"\n");
        let _ = file.flush();
    }
}

fn trace_session_event(
    event: &str,
    bead_id: &BeadId,
    run_id: &RunId,
    session_id: &grove_types::SessionId,
    fields: serde_json::Value,
) {
    write_trace_event(serde_json::json!({
        "event": event,
        "bead_id": bead_id.as_str(),
        "run_id": run_id.as_str(),
        "session_id": session_id.as_str(),
        "fields": fields,
    }));
}

pub fn trace_runtime_event(event: &str, fields: serde_json::Value) {
    write_trace_event(serde_json::json!({
        "event": event,
        "fields": fields,
    }));
}

pub fn execute_persisted_single_task_session<B: ClaudeBackend>(
    db: &mut Database,
    backend: &B,
    request: SingleTaskSessionRequest,
    attempt_no: i32,
    config: &GroveConfig,
) -> Result<PersistedTaskRunOutcome> {
    let bead_id = request.bead_id.clone();
    let run_id = request.run_id.clone();
    let session_id = request.session_id.clone();
    let checkpoint_root = request
        .working_dir
        .join(DEFAULT_GROVE_DIR_NAME)
        .join(DEFAULT_CHECKPOINTS_DIR_NAME);
    let started_at = chrono::Utc::now();

    let escalation_tier = request.escalation_tier;
    db.record_run_started(RunStartInput {
        run_id: run_id.clone(),
        bead_id: bead_id.clone(),
        attempt_no,
        started_at,
        escalation_tier,
    })?;

    execute_persisted_single_task_session_after_run_started(
        db,
        backend,
        request,
        attempt_no,
        config,
        bead_id,
        run_id,
        session_id,
        checkpoint_root.into_std_path_buf(),
    )
}

/// Execute a persisted single task session when the run has already been recorded.
/// This is used by the dispatch loop to avoid concurrent SQLite writes by recording
/// the run start on the main thread before spawning workers.
#[allow(clippy::too_many_arguments)]
pub fn execute_persisted_single_task_session_after_run_started<B: ClaudeBackend>(
    db: &mut Database,
    backend: &B,
    request: SingleTaskSessionRequest,
    _attempt_no: i32,
    config: &GroveConfig,
    bead_id: BeadId,
    run_id: RunId,
    session_id: grove_types::SessionId,
    checkpoint_root: PathBuf,
) -> Result<PersistedTaskRunOutcome> {
    let mut hooks = DbSessionLifecycleHooks::new(
        db,
        bead_id.clone(),
        run_id.clone(),
        session_id.clone(),
        checkpoint_root,
    );
    let result = execute_single_task_session_with_hooks(backend, request, &mut hooks);

    match result {
        Ok(result) => {
            trace_session_event(
                "session.run_result",
                &bead_id,
                &run_id,
                &session_id,
                serde_json::json!({
                    "session_status": format!("{:?}", result.outcome.session.status),
                    "terminal_class": format!("{:?}", result.outcome.terminal_class),
                    "stop_reason": result.outcome.session.stop_reason.map(|reason| format!("{:?}", reason)),
                    "exit_code": result.outcome.session.exit_code,
                }),
            );
            let run =
                finalize_persisted_run(hooks.db_mut(), &bead_id, &result.outcome, None, config)?;
            let checkpoint_root = hooks.checkpoint_root().to_path_buf();
            let checkpoint = if let Some(checkpoint) = hooks.take_checkpoint() {
                Some(checkpoint)
            } else {
                persist_fallback_checkpoint(
                    hooks.db_mut(),
                    &checkpoint_root,
                    &bead_id,
                    &run_id,
                    &session_id,
                    &result.outcome,
                    run.failure_class,
                    run.failure_detail.as_deref(),
                )?
            };
            let handoff = persist_success_handoff(hooks.db_mut(), &bead_id, &result.outcome)?;
            Ok(PersistedTaskRunOutcome {
                run,
                session: result.outcome,
                checkpoint,
                handoff,
            })
        }
        Err(error) => {
            trace_session_event(
                "session.run_error",
                &bead_id,
                &run_id,
                &session_id,
                serde_json::json!({
                    "error": error.to_string(),
                    "had_latest_outcome": hooks.latest_outcome().is_some(),
                }),
            );
            if let Some(outcome) = hooks.latest_outcome() {
                let _ = finalize_persisted_run(
                    hooks.db_mut(),
                    &bead_id,
                    &outcome,
                    Some(error.to_string()),
                    config,
                );
            } else {
                let _ = hooks.db_mut().record_run_finished(
                    &bead_id,
                    RunFinishInput {
                        run_id,
                        status: RunStatus::Failed,
                        failure_class: Some(FailureClass::Unknown),
                        failure_detail: Some(error.to_string()),
                        ended_at: chrono::Utc::now(),
                        retry_after: None,
                        circuit_breaker_state: None,
                    },
                );
            }
            Err(error.into())
        }
    }
}

struct DbSessionLifecycleHooks<'a> {
    db: &'a mut Database,
    bead_id: BeadId,
    run_id: RunId,
    session_id: grove_types::SessionId,
    checkpoint_root: PathBuf,
    checkpoint: Option<grove_types::CheckpointRecord>,
    latest_outcome: Option<grove_types::SessionOutcome>,
}

impl<'a> DbSessionLifecycleHooks<'a> {
    fn new(
        db: &'a mut Database,
        bead_id: BeadId,
        run_id: RunId,
        session_id: grove_types::SessionId,
        checkpoint_root: PathBuf,
    ) -> Self {
        Self {
            db,
            bead_id,
            run_id,
            session_id,
            checkpoint_root,
            checkpoint: None,
            latest_outcome: None,
        }
    }

    fn db_mut(&mut self) -> &mut Database {
        self.db
    }

    fn take_checkpoint(&mut self) -> Option<grove_types::CheckpointRecord> {
        self.checkpoint.take()
    }

    fn latest_outcome(&self) -> Option<grove_types::SessionOutcome> {
        self.latest_outcome.clone()
    }

    fn checkpoint_path(&self, checkpoint_id: &CheckpointId) -> PathBuf {
        self.checkpoint_root
            .join(self.bead_id.as_str())
            .join(format!("{}.json", checkpoint_id.as_str()))
    }

    fn checkpoint_root(&self) -> &Path {
        &self.checkpoint_root
    }
}

fn persist_checkpoint_file(path: &Path, checkpoint: &grove_types::CheckpointRecord) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create checkpoint directory {}", parent.display()))?;
    }

    let encoded = serde_json::to_vec_pretty(checkpoint)
        .with_context(|| format!("encode checkpoint JSON {}", path.display()))?;
    let temp_path = path.with_extension("json.tmp");
    fs::write(&temp_path, encoded)
        .with_context(|| format!("write checkpoint temp file {}", temp_path.display()))?;
    fs::rename(&temp_path, path).with_context(|| {
        format!(
            "rename checkpoint temp file {} to {}",
            temp_path.display(),
            path.display()
        )
    })?;
    Ok(())
}

impl SessionLifecycleHooks for DbSessionLifecycleHooks<'_> {
    fn on_session_started(
        &mut self,
        session: &grove_types::ClaudeSessionRecord,
    ) -> anyhow::Result<()> {
        self.db.record_session_started(&self.bead_id, session)?;
        trace_session_event(
            "session.started",
            &self.bead_id,
            &self.run_id,
            &self.session_id,
            serde_json::json!({
                "status": format!("{:?}", session.status),
                "ordinal_in_run": session.ordinal_in_run,
                "transcript_path": session.transcript_path.clone(),
            }),
        );
        Ok(())
    }

    fn on_activity_changed(
        &mut self,
        activity: AgentActivity,
        detail: Option<&str>,
        at: chrono::DateTime<chrono::Utc>,
    ) -> anyhow::Result<()> {
        self.db
            .update_run_activity(&self.bead_id, &self.run_id, activity, &at)?;
        if let Some(detail) = detail {
            self.db.write_event_log(
                grove_types::EventKind::ActivityStateChanged,
                Some(&self.bead_id),
                Some(&self.run_id),
                Some(&self.session_id),
                &serde_json::json!({
                    "activity": activity,
                    "detail": detail,
                }),
                &at,
            )?;
        }
        trace_session_event(
            "session.activity_changed",
            &self.bead_id,
            &self.run_id,
            &self.session_id,
            serde_json::json!({
                "activity": format!("{:?}", activity),
                "detail": detail,
                "at": at,
            }),
        );
        Ok(())
    }

    fn on_shutdown_requested(
        &mut self,
        grace_period: Option<std::time::Duration>,
    ) -> anyhow::Result<()> {
        self.db.write_event_log(
            grove_types::EventKind::SessionTerminationRequested,
            Some(&self.bead_id),
            Some(&self.run_id),
            Some(&self.session_id),
            &serde_json::json!({
                "grace_period_ms": grace_period.map(|duration| duration.as_millis() as u64),
            }),
            &chrono::Utc::now(),
        )?;
        trace_session_event(
            "session.shutdown_requested",
            &self.bead_id,
            &self.run_id,
            &self.session_id,
            serde_json::json!({
                "grace_period_ms": grace_period.map(|duration| duration.as_millis() as u64),
            }),
        );
        Ok(())
    }

    fn on_shutdown_forced(&mut self) -> anyhow::Result<()> {
        self.db.write_event_log(
            grove_types::EventKind::SessionTerminationForced,
            Some(&self.bead_id),
            Some(&self.run_id),
            Some(&self.session_id),
            &serde_json::json!({"forced": true}),
            &chrono::Utc::now(),
        )?;
        trace_session_event(
            "session.shutdown_forced",
            &self.bead_id,
            &self.run_id,
            &self.session_id,
            serde_json::json!({"forced": true}),
        );
        Ok(())
    }

    fn on_session_finished(&mut self, result: &SingleTaskSessionResult) -> anyhow::Result<()> {
        self.db
            .record_session_finished(&self.bead_id, &result.outcome.session)?;
        trace_session_event(
            "session.finished",
            &self.bead_id,
            &self.run_id,
            &self.session_id,
            serde_json::json!({
                "session_status": format!("{:?}", result.outcome.session.status),
                "terminal_class": format!("{:?}", result.outcome.terminal_class),
                "stop_reason": result.outcome.session.stop_reason.map(|reason| format!("{:?}", reason)),
                "exit_code": result.outcome.session.exit_code,
                "result_summary": result.protocol_state.result_summary.clone(),
                "artifacts": result.protocol_state.artifacts.clone(),
                "lessons": result.protocol_state.lessons.clone(),
                "decisions": result.protocol_state.decisions.clone(),
                "warnings": result.protocol_state.warnings.clone(),
            }),
        );

        if let Some(payload) = result.protocol_state.latest_checkpoint.clone() {
            let checkpoint_id = CheckpointId::new(format!(
                "chk-{}-{}",
                self.run_id.as_str(),
                result.outcome.session.ordinal_in_run
            ));
            let checkpoint = self.db.record_checkpoint_saved(SessionCheckpointInput {
                checkpoint_id: checkpoint_id.clone(),
                bead_id: self.bead_id.clone(),
                run_id: self.run_id.clone(),
                session_id: self.session_id.clone(),
                payload,
                saved_at: result
                    .outcome
                    .session
                    .ended_at
                    .unwrap_or_else(chrono::Utc::now),
                resume_generation: result.outcome.session.ordinal_in_run as u32,
            })?;
            let checkpoint_path = self.checkpoint_path(&checkpoint_id);
            if let Err(error) = persist_checkpoint_file(&checkpoint_path, &checkpoint) {
                self.latest_outcome = Some(result.outcome.clone());
                trace_session_event(
                    "session.checkpoint_persist_error",
                    &self.bead_id,
                    &self.run_id,
                    &self.session_id,
                    serde_json::json!({
                        "path": checkpoint_path.display().to_string(),
                        "error": error.to_string(),
                    }),
                );
                return Err(CheckpointFilePersistError {
                    path: checkpoint_path,
                    source: error.to_string(),
                }
                .into());
            }
            self.checkpoint = Some(checkpoint);
        }

        if let Ok(replay) =
            grove_session::replay_transcript(&result.outcome.session.transcript_path)
            && let Ok(mut archived) = crate::archive::ingest_transcript_to_archive(
                self.bead_id.clone(),
                self.run_id.clone(),
                self.session_id.clone(),
                &replay,
            )
        {
            archived.source_path =
                camino::Utf8PathBuf::from(result.outcome.session.transcript_path.clone());

            let source_record = grove_types::archive::SourceRecord {
                id: grove_types::SourceId::new("transcript"),
                source_path: archived.source_path.clone(),
                origin_host: None,
                metadata_json: serde_json::json!({}),
            };
            let _ = self.db.insert_source_record(&source_record);
            // Idempotent: skips if this session was already ingested (watermark check)
            let _ = self.db.insert_conversation_idempotent(&archived);
        }

        // Ingest GROVE_LESSONS from protocol state into playbook as draft candidates
        if !result.protocol_state.lessons.is_empty() {
            let _ = crate::lesson_ingest::ingest_lessons(
                self.db,
                &self.bead_id,
                &self.run_id,
                &result.protocol_state.lessons,
            );
        }

        // Apply implicit outcome feedback to any playbook bullets injected during this session
        let _ = crate::diary::apply_outcome_feedback(
            self.db,
            &self.bead_id,
            &self.run_id,
            &result.outcome,
            result.outcome.session.ordinal_in_run > 1,
        );

        self.latest_outcome = Some(result.outcome.clone());
        Ok(())
    }
}

#[cfg(all(test, unix))]
fn read_trace_log_lines(workspace_dir: &camino::Utf8Path) -> Result<Vec<serde_json::Value>> {
    let path = workspace_dir
        .join(DEFAULT_GROVE_DIR_NAME)
        .join(DEFAULT_LOGS_DIR_NAME)
        .join("runtime.jsonl");
    let content = fs::read_to_string(path.as_std_path())?;
    content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(serde_json::from_str)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn normalize_runtime_line(line: &str) -> String {
    line.split_whitespace()
        .map(|part| part.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(" ")
}

fn has_plan_approval_detour(lines: &[String]) -> bool {
    lines.iter().any(|line| {
        let normalized = normalize_runtime_line(line);
        normalized.contains("requested approval")
            || normalized.contains("plan file")
            || normalized.contains("plan mode")
    })
}

fn unknown_failure_should_retry(outcome: &grove_types::SessionOutcome) -> bool {
    outcome.session.exit_code.is_none()
        || outcome.analysis.repeated_error_fingerprint.is_some()
        || !matches!(outcome.analysis.probable_progress, ProgressSignal::None)
        || grove_session::contains_invalid_image_input(&outcome.stdout_tail, &outcome.stderr_tail)
        || has_plan_approval_detour(&outcome.stdout_tail)
        || has_plan_approval_detour(&outcome.stderr_tail)
}

fn finalize_persisted_run(
    db: &mut Database,
    bead_id: &BeadId,
    outcome: &grove_types::SessionOutcome,
    failure_detail_override: Option<String>,
    config: &GroveConfig,
) -> Result<grove_types::TaskRunRecord> {
    let ended_at = outcome.session.ended_at.unwrap_or_else(chrono::Utc::now);
    let forced_kill = outcome.session.stop_reason == Some(grove_types::StopReason::Kill);
    let (status, failure_class, retry_after) = match outcome.session.status {
        SessionStatus::Checkpointed => (RunStatus::Checkpointed, None, None),
        SessionStatus::Completed => (RunStatus::Succeeded, None, None),
        SessionStatus::TimedOut => (
            RunStatus::WaitingToRetry,
            Some(FailureClass::Timeout),
            Some(ended_at),
        ),
        SessionStatus::RateLimited => (
            RunStatus::WaitingToRetry,
            Some(FailureClass::RateLimit),
            Some(ended_at),
        ),
        SessionStatus::PermissionDenied => (
            RunStatus::Failed,
            Some(FailureClass::PermissionDenied),
            None,
        ),
        SessionStatus::Crashed => (RunStatus::Failed, Some(FailureClass::ClaudeCrashed), None),
        SessionStatus::UnknownFailure if forced_kill => (
            RunStatus::WaitingToRetry,
            Some(FailureClass::Interrupted),
            Some(ended_at),
        ),
        SessionStatus::UnknownFailure if unknown_failure_should_retry(outcome) => (
            RunStatus::WaitingToRetry,
            Some(FailureClass::NoProgress),
            Some(ended_at),
        ),
        SessionStatus::UnknownFailure => (RunStatus::Failed, Some(FailureClass::Unknown), None),
        SessionStatus::Starting | SessionStatus::Running => {
            (RunStatus::Failed, Some(FailureClass::Unknown), None)
        }
    };
    let failure_detail = failure_detail_override.or_else(|| {
        outcome
            .session
            .stop_reason
            .map(|reason| format!("session ended with {:?}", reason))
    });
    let prior_breaker = db
        .get_bead_record(bead_id)?
        .and_then(|record| record.circuit_breaker_state)
        .unwrap_or_default();
    let circuit_breaker_state = update_circuit_breaker(
        prior_breaker,
        &outcome.analysis,
        ended_at,
        config.circuit_breaker.no_progress_threshold,
        config.circuit_breaker.same_error_threshold,
        config.circuit_breaker.permission_denial_threshold,
        config.circuit_breaker.cooldown_minutes,
    );

    let run = db.record_run_finished(
        bead_id,
        RunFinishInput {
            run_id: outcome.session.run_id.clone(),
            status,
            failure_class,
            failure_detail: failure_detail.clone(),
            ended_at,
            retry_after,
            circuit_breaker_state: Some(circuit_breaker_state),
        },
    )?;

    if let Some(capsule) =
        recovery_capsule_from_outcome(outcome, failure_class, failure_detail.as_deref())
    {
        db.write_recovery_capsule(RecoveryCapsuleWriteInput {
            bead_id: bead_id.clone(),
            run_id: outcome.session.run_id.clone(),
            capsule,
            created_at: ended_at,
        })?;
    }

    Ok(run)
}

fn synthetic_checkpoint_payload_from_outcome(
    outcome: &grove_types::SessionOutcome,
    failure_class: Option<FailureClass>,
    failure_detail: Option<&str>,
) -> Option<grove_types::CheckpointPayload> {
    if outcome.session.status != SessionStatus::Crashed
        && outcome.session.status != SessionStatus::UnknownFailure
    {
        return None;
    }
    if outcome
        .protocol_events
        .iter()
        .any(|event| matches!(event, grove_types::ProtocolEvent::Checkpoint { .. }))
    {
        return None;
    }

    let failure_label = failure_class
        .map(|class| format!("{:?}", class))
        .unwrap_or_else(|| "Unknown".to_owned());
    let detail = failure_detail
        .filter(|detail| !detail.trim().is_empty())
        .unwrap_or("session ended before Grove captured a structured checkpoint");

    Some(grove_types::CheckpointPayload {
        progress: format!("Synthetic fallback checkpoint after {}", failure_label),
        next_step: "Resume from the transcript tail, avoid repeating completed setup, and continue with the smallest safe remaining step.".to_owned(),
        context: serde_json::json!({
            "synthetic": true,
            "failure_class": failure_class.map(|class| format!("{:?}", class)),
            "failure_detail": detail,
            "stdout_tail": outcome.stdout_tail,
            "stderr_tail": outcome.stderr_tail,
        }),
        open_questions: Vec::new(),
        claimed_paths: Vec::new(),
        confidence: Some(0.25),
    })
}

#[allow(clippy::too_many_arguments)]
fn persist_fallback_checkpoint(
    db: &mut Database,
    checkpoint_root: &std::path::Path,
    bead_id: &BeadId,
    run_id: &RunId,
    session_id: &grove_types::SessionId,
    outcome: &grove_types::SessionOutcome,
    failure_class: Option<FailureClass>,
    failure_detail: Option<&str>,
) -> Result<Option<grove_types::CheckpointRecord>> {
    let Some(payload) =
        synthetic_checkpoint_payload_from_outcome(outcome, failure_class, failure_detail)
    else {
        return Ok(None);
    };
    let saved_at = outcome.session.ended_at.unwrap_or_else(chrono::Utc::now);
    let checkpoint_id = CheckpointId::new(format!(
        "chk-{}-fallback-{}",
        run_id.as_str(),
        outcome.session.ordinal_in_run
    ));
    let checkpoint = db.record_checkpoint_saved(SessionCheckpointInput {
        checkpoint_id: checkpoint_id.clone(),
        bead_id: bead_id.clone(),
        run_id: run_id.clone(),
        session_id: session_id.clone(),
        payload,
        saved_at,
        resume_generation: outcome.session.ordinal_in_run as u32,
    })?;
    let checkpoint_path = checkpoint_root
        .join(bead_id.as_str())
        .join(format!("{}.json", checkpoint_id.as_str()));
    persist_checkpoint_file(&checkpoint_path, &checkpoint)?;
    Ok(Some(checkpoint))
}

fn recovery_capsule_from_outcome(
    outcome: &grove_types::SessionOutcome,
    failure_class: Option<FailureClass>,
    failure_detail: Option<&str>,
) -> Option<grove_types::RecoveryCapsule> {
    let outcome_kind = match outcome.session.status {
        SessionStatus::Checkpointed => grove_types::RecoveryCapsuleOutcome::Checkpointed,
        SessionStatus::TimedOut
        | SessionStatus::RateLimited
        | SessionStatus::PermissionDenied
        | SessionStatus::Crashed
        | SessionStatus::UnknownFailure
        | SessionStatus::Starting
        | SessionStatus::Running => grove_types::RecoveryCapsuleOutcome::Failed,
        SessionStatus::Completed => return None,
    };

    let checkpoint = outcome
        .protocol_events
        .iter()
        .rev()
        .find_map(|event| match event {
            grove_types::ProtocolEvent::Checkpoint { payload } => Some(payload),
            _ => None,
        });
    let artifacts = outcome
        .protocol_events
        .iter()
        .rev()
        .find_map(|event| match event {
            grove_types::ProtocolEvent::Artifacts { items } => Some(items.as_slice()),
            _ => None,
        })
        .unwrap_or(&[]);

    let mut enriched_detail = failure_detail.unwrap_or_default().to_string();
    if !outcome.stderr_tail.is_empty() {
        if !enriched_detail.is_empty() {
            enriched_detail.push_str("\n\n");
        }
        enriched_detail.push_str("Recent stderr:\n");
        enriched_detail.push_str(&outcome.stderr_tail.join("\n"));
    }

    grove_types::RecoveryCapsule::from_parts(
        outcome_kind,
        failure_class,
        if enriched_detail.is_empty() {
            None
        } else {
            Some(enriched_detail.as_str())
        },
        checkpoint.map(|payload| payload.progress.as_str()),
        checkpoint.map(|payload| payload.next_step.as_str()),
        None,
        outcome.session.prompt_manifest_path.as_ref().and({
            // Note: In a complete implementation, we'd load the prompt manifest
            // to fetch `retry_delta_summary`. For now we rely on it being populated
            // via the with_retry_context in the dispatch loop.
            None
        }),
        artifacts,
    )
}

fn persist_success_handoff(
    db: &mut Database,
    bead_id: &BeadId,
    outcome: &grove_types::SessionOutcome,
) -> Result<Option<grove_types::HandoffRecord>> {
    if outcome.session.status != SessionStatus::Completed {
        return Ok(None);
    }

    let Some(summary) = outcome
        .protocol_events
        .iter()
        .find_map(|event| match event {
            grove_types::ProtocolEvent::Result { summary } => Some(summary.clone()),
            _ => None,
        })
        .or_else(|| outcome.stdout_tail.last().cloned())
    else {
        return Ok(None);
    };

    let completed_at = outcome.session.ended_at.unwrap_or_else(chrono::Utc::now);
    db.write_handoff(HandoffWriteInput {
        bead_id: bead_id.clone(),
        run_id: outcome.session.run_id.clone(),
        summary,
        artifacts: outcome
            .protocol_events
            .iter()
            .find_map(|event| match event {
                grove_types::ProtocolEvent::Artifacts { items } => Some(items.clone()),
                _ => None,
            })
            .unwrap_or_default(),
        lessons: outcome
            .protocol_events
            .iter()
            .find_map(|event| match event {
                grove_types::ProtocolEvent::Lessons { items } => Some(items.clone()),
                _ => None,
            })
            .unwrap_or_default(),
        decisions: outcome
            .protocol_events
            .iter()
            .find_map(|event| match event {
                grove_types::ProtocolEvent::Decisions { items } => Some(items.clone()),
                _ => None,
            })
            .unwrap_or_default(),
        warnings: outcome
            .protocol_events
            .iter()
            .find_map(|event| match event {
                grove_types::ProtocolEvent::Warnings { items } => Some(items.clone()),
                _ => None,
            })
            .unwrap_or_default(),
        completed_at,
    })
    .map(Some)
}

pub fn parent_handoff_summaries(db: &Database, bead_id: &BeadId) -> Result<Vec<String>> {
    db.parent_handoffs_for_bead(bead_id).map(|handoffs| {
        handoffs
            .into_iter()
            .map(|handoff| {
                let mut lines = vec![format!(
                    "Parent {} (run {}) prepared this task: {}",
                    handoff.bead_id, handoff.run_id, handoff.summary
                )];
                if !handoff.artifacts.is_empty() {
                    lines.push(format!("Artifacts: {}", handoff.artifacts.join(", ")));
                }
                if !handoff.decisions.is_empty() {
                    lines.push(format!("Decisions: {}", handoff.decisions.join(" | ")));
                }
                if !handoff.lessons.is_empty() {
                    lines.push(format!("Lessons: {}", handoff.lessons.join(" | ")));
                }
                if !handoff.warnings.is_empty() {
                    lines.push(format!("Warnings: {}", handoff.warnings.join(" | ")));
                }
                lines.join("\n")
            })
            .collect()
    })
}

#[derive(Debug, Clone)]
pub struct AcquireReservationInput {
    pub path_pattern: String,
    pub mode: ReservationMode,
    pub reason: Option<String>,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone)]
pub struct ReservationManager;

impl ReservationManager {
    pub fn acquire_for_run(
        db: &mut Database,
        bead_id: &BeadId,
        run_id: Option<&RunId>,
        requests: &[AcquireReservationInput],
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<ReservationAcquireOutcome> {
        let requests = requests
            .iter()
            .map(|request| ReservationRequest {
                path_pattern: request.path_pattern.as_str(),
                mode: request.mode,
                reason: request.reason.as_deref(),
                expires_at: request.expires_at,
            })
            .collect::<Vec<_>>();
        db.acquire_reservations(bead_id, run_id, &requests, &now)
    }

    pub fn release_for_run(
        db: &mut Database,
        bead_id: &BeadId,
        run_id: &RunId,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<ReservationRecord>> {
        db.release_reservations_for_run(bead_id, run_id, &now)
    }

    pub fn release_for_bead(
        db: &mut Database,
        bead_id: &BeadId,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<ReservationRecord>> {
        db.release_reservations_for_bead(bead_id, &now)
    }

    pub fn reconcile(
        db: &mut Database,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<ReservationReconcileReport> {
        let expired = db.expire_reservations(&now)?;
        let recovered = db.recover_stale_reservations(&now)?;
        Ok(ReservationReconcileReport { expired, recovered })
    }
}

#[derive(Debug, Clone)]
pub struct ReservationReconcileReport {
    pub expired: Vec<ReservationRecord>,
    pub recovered: Vec<RecoveredReservation>,
}

#[derive(Debug, Clone)]
pub struct LeaderLeaseConfig {
    pub owner_label: String,
    pub lease_ttl: chrono::Duration,
}

#[derive(Debug, Clone)]
pub struct StartupRecoveryReport {
    pub interrupted_runs: Vec<InterruptedRunRecovery>,
    pub reservations: ReservationReconcileReport,
}

#[derive(Debug, Clone)]
pub struct StartupCoordinatorState {
    pub leader: grove_types::LeaderLeaseRecord,
    pub recovery: StartupRecoveryReport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaderLeaseAcquireError {
    Contested { owner_label: String },
}

impl std::fmt::Display for LeaderLeaseAcquireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Contested { owner_label } => {
                write!(
                    f,
                    "leader lease conflict: another leader is active (owner: {owner_label})"
                )
            }
        }
    }
}

impl std::error::Error for LeaderLeaseAcquireError {}

#[derive(Debug, Clone)]
pub struct LeaderLeaseManager;

impl LeaderLeaseManager {
    pub fn acquire(
        db: &mut Database,
        config: &LeaderLeaseConfig,
        run_id: Option<&RunId>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> std::result::Result<grove_types::LeaderLeaseRecord, LeaderLeaseAcquireError> {
        let expires_at = now + config.lease_ttl;
        match db.acquire_leader_lease(LeaderLeaseAcquireInput {
            owner_label: config.owner_label.clone(),
            run_id: run_id.cloned(),
            acquired_at: now,
            expires_at,
        }) {
            Ok(Some(lease)) => Ok(lease),
            Ok(None) => {
                let owner_label = db
                    .active_leader_lease(&now)
                    .ok()
                    .flatten()
                    .map(|lease| lease.owner_label)
                    .unwrap_or_else(|| "unknown".to_owned());
                Err(LeaderLeaseAcquireError::Contested { owner_label })
            }
            Err(error) => Err(LeaderLeaseAcquireError::Contested {
                owner_label: error.to_string(),
            }),
        }
    }

    pub fn heartbeat(
        db: &mut Database,
        config: &LeaderLeaseConfig,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<grove_types::LeaderLeaseRecord>> {
        let expires_at = now + config.lease_ttl;
        db.heartbeat_leader_lease(&config.owner_label, &now, &expires_at)
    }

    pub fn release(
        db: &mut Database,
        owner_label: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<grove_types::LeaderLeaseRecord>> {
        db.release_leader_lease(owner_label, &now)
    }
}

pub fn reconcile_startup_state(
    db: &mut Database,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<StartupRecoveryReport> {
    let interrupted_runs = db.reconcile_interrupted_runs(&now)?;
    let reservations = ReservationManager::reconcile(db, now)?;
    Ok(StartupRecoveryReport {
        interrupted_runs,
        reservations,
    })
}

pub fn acquire_startup_coordinator(
    db: &mut Database,
    config: &LeaderLeaseConfig,
    run_id: Option<&RunId>,
    now: chrono::DateTime<chrono::Utc>,
) -> std::result::Result<StartupCoordinatorState, LeaderLeaseAcquireError> {
    let leader = LeaderLeaseManager::acquire(db, config, run_id, now)?;
    let recovery = match reconcile_startup_state(db, now) {
        Ok(recovery) => recovery,
        Err(error) => {
            let _ = LeaderLeaseManager::release(db, &config.owner_label, now);
            return Err(LeaderLeaseAcquireError::Contested {
                owner_label: error.to_string(),
            });
        }
    };
    Ok(StartupCoordinatorState { leader, recovery })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DependencySnapshotIssue {
    SelfBlockedBy,
    SelfBlocks,
    DuplicateBlockedBy { bead_id: BeadId, occurrences: usize },
    DuplicateBlocks { bead_id: BeadId, occurrences: usize },
}

impl DependencySnapshotIssue {
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::SelfBlockedBy => "self_blocked_by",
            Self::SelfBlocks => "self_blocks",
            Self::DuplicateBlockedBy { .. } => "duplicate_blocked_by",
            Self::DuplicateBlocks { .. } => "duplicate_blocks",
        }
    }

    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            Self::SelfBlockedBy => {
                "cached dependency snapshot lists the bead as blocking itself".to_owned()
            }
            Self::SelfBlocks => {
                "cached dependency snapshot lists the bead as its own dependent".to_owned()
            }
            Self::DuplicateBlockedBy {
                bead_id,
                occurrences,
            } => format!(
                "cached dependency snapshot repeats blocker {} {} times",
                bead_id.as_str(),
                occurrences
            ),
            Self::DuplicateBlocks {
                bead_id,
                occurrences,
            } => format!(
                "cached dependency snapshot repeats dependent {} {} times",
                bead_id.as_str(),
                occurrences
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DependencySnapshotSanity {
    pub snapshot: BrDependencySnapshot,
    pub issues: Vec<DependencySnapshotIssue>,
}

impl DependencySnapshotSanity {
    #[must_use]
    pub fn is_sane(&self) -> bool {
        self.issues.is_empty()
    }
}

#[must_use]
pub fn validate_dependency_snapshot(
    snapshot: &BrDependencySnapshot,
) -> Vec<DependencySnapshotIssue> {
    let mut issues = Vec::new();

    if snapshot.blocked_by.iter().any(|id| id == &snapshot.bead_id) {
        issues.push(DependencySnapshotIssue::SelfBlockedBy);
    }

    if snapshot.blocks.iter().any(|id| id == &snapshot.bead_id) {
        issues.push(DependencySnapshotIssue::SelfBlocks);
    }

    issues.extend(
        duplicate_dependency_ids(&snapshot.blocked_by)
            .into_iter()
            .map(
                |(bead_id, occurrences)| DependencySnapshotIssue::DuplicateBlockedBy {
                    bead_id,
                    occurrences,
                },
            ),
    );
    issues.extend(duplicate_dependency_ids(&snapshot.blocks).into_iter().map(
        |(bead_id, occurrences)| DependencySnapshotIssue::DuplicateBlocks {
            bead_id,
            occurrences,
        },
    ));

    issues
}

pub fn inspect_dependency_snapshot(
    db: &Database,
    bead_id: &BeadId,
) -> Result<Option<DependencySnapshotSanity>> {
    if db.get_bead_record(bead_id)?.is_none() {
        return Ok(None);
    }

    let snapshot = db.dependency_snapshot(bead_id)?;
    let issues = validate_dependency_snapshot(&snapshot);
    Ok(Some(DependencySnapshotSanity { snapshot, issues }))
}

pub fn load_workspace_status_view<C: BrClient>(
    db: &Database,
    br: &C,
    workspace_root: &str,
    config: &GroveConfig,
    triage: Option<&BvTriageOutput>,
) -> Result<WorkspaceStatusView> {
    Ok(status_view::load_status_snapshot(db, br, workspace_root, config, triage)?.into_view())
}

pub fn load_bead_inspect_view<C: BrClient>(
    db: &Database,
    br: &C,
    bead_id: &BeadId,
    workspace_root: &str,
    config: &GroveConfig,
    triage: Option<&BvTriageOutput>,
) -> Result<Option<BeadInspectView>> {
    Ok(
        inspect_view::load_inspect_snapshot(db, br, bead_id, workspace_root, config, triage)?
            .map(|snapshot| snapshot.into_view()),
    )
}

fn duplicate_dependency_ids(ids: &[BeadId]) -> Vec<(BeadId, usize)> {
    let mut counts = BTreeMap::<String, usize>::new();
    for bead_id in ids {
        *counts.entry(bead_id.as_str().to_owned()).or_default() += 1;
    }

    counts
        .into_iter()
        .filter(|(_, occurrences)| *occurrences > 1)
        .map(|(bead_id, occurrences)| (BeadId::new(bead_id), occurrences))
        .collect()
}

#[derive(Debug, Clone)]
pub struct DispatchEligibilityContext {
    pub ready_in_br: bool,
    pub circuit_state: CircuitState,
    pub reservation_conflicts: Vec<ReservationConflict>,
    pub now: Timestamp,
}

#[derive(Debug, Clone)]
pub struct DispatchEligibility {
    pub ready_in_br: bool,
    pub dispatchable_in_grove: bool,
    pub local_suppression_reasons: Vec<LocalSuppressionReason>,
}

impl DispatchEligibility {
    #[must_use]
    pub fn has_local_suppressions(&self) -> bool {
        !self.local_suppression_reasons.is_empty()
    }
}

#[derive(Debug, Clone)]
pub enum LocalSuppressionReason {
    SuppressedByLabel { label: String },
    ActiveRun { run_id: Option<RunId> },
    CheckpointPendingResume { run_id: Option<RunId> },
    RetryBackoffPending { retry_after: Option<Timestamp> },
    CircuitOpen,
    ReservationConflict { conflict: ReservationConflict },
    AlreadySucceeded,
    FailedAwaitingManualRetry,
}

impl LocalSuppressionReason {
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::SuppressedByLabel { .. } => "suppressed_by_label",
            Self::ActiveRun { .. } => "active_run",
            Self::CheckpointPendingResume { .. } => "checkpoint_pending_resume",
            Self::RetryBackoffPending { .. } => "retry_backoff_pending",
            Self::CircuitOpen => "circuit_open",
            Self::ReservationConflict { .. } => "reservation_conflict",
            Self::AlreadySucceeded => "already_succeeded",
            Self::FailedAwaitingManualRetry => "failed_awaiting_manual_retry",
        }
    }
}

#[must_use]
pub fn evaluate_dispatch_eligibility(
    bead: &GroveBeadRecord,
    context: &DispatchEligibilityContext,
) -> DispatchEligibility {
    let local_suppression_reasons = collect_local_suppressions(bead, context);
    let dispatchable_in_grove = context.ready_in_br && local_suppression_reasons.is_empty();

    DispatchEligibility {
        ready_in_br: context.ready_in_br,
        dispatchable_in_grove,
        local_suppression_reasons,
    }
}

#[must_use]
pub fn dispatch_suppression_label(labels: &[String]) -> Option<String> {
    labels
        .iter()
        .find(|label| label.eq_ignore_ascii_case("dispatch:no"))
        .cloned()
}

#[must_use]
pub fn circuit_state_for_bead(bead: &GroveBeadRecord) -> CircuitState {
    bead.circuit_breaker_state
        .as_ref()
        .map(|state| state.state)
        .unwrap_or(CircuitState::Closed)
}

fn collect_local_suppressions(
    bead: &GroveBeadRecord,
    context: &DispatchEligibilityContext,
) -> Vec<LocalSuppressionReason> {
    let mut reasons = Vec::new();

    if let Some(label) = dispatch_suppression_label(&bead.bead.labels) {
        reasons.push(LocalSuppressionReason::SuppressedByLabel { label });
    }

    match bead.grove_status {
        GroveBeadStatus::Idle | GroveBeadStatus::Ready => {}
        GroveBeadStatus::Running => reasons.push(LocalSuppressionReason::ActiveRun {
            run_id: bead.last_run_id.clone(),
        }),
        GroveBeadStatus::Checkpointed => {}
        GroveBeadStatus::WaitingToRetry => {
            if bead.retry_after.is_none()
                || bead
                    .retry_after
                    .as_ref()
                    .is_some_and(|ts| ts > &context.now)
            {
                reasons.push(LocalSuppressionReason::RetryBackoffPending {
                    retry_after: bead.retry_after,
                });
            }
        }
        GroveBeadStatus::Succeeded => reasons.push(LocalSuppressionReason::AlreadySucceeded),
        GroveBeadStatus::Failed => reasons.push(LocalSuppressionReason::FailedAwaitingManualRetry),
    }

    if matches!(context.circuit_state, CircuitState::Open) {
        reasons.push(LocalSuppressionReason::CircuitOpen);
    }

    reasons.extend(
        context
            .reservation_conflicts
            .iter()
            .cloned()
            .map(|conflict| LocalSuppressionReason::ReservationConflict { conflict }),
    );

    reasons
}

#[cfg(test)]
mod tests;
