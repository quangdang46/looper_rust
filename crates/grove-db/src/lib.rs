use std::{fs, path::PathBuf};

use anyhow::{bail, Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use chrono::{NaiveDateTime, TimeZone, Utc};
use glob::{MatchOptions, Pattern};
use grove_br::{
    BeadCacheStore, BrDependencySnapshot, BrIssueSummary, CachedBeadState, UpsertOutcome,
};
use grove_types::{
    BeadId, BeadPriority, BeadRef, CheckpointId, CheckpointPayload, CheckpointRecord,
    CircuitBreakerState, ClaudeSessionRecord, ContextSnapshot, EventError, EventKind,
    EventLogRecord, EventOutcome, FailureClass, GroveBeadRecord, GroveBeadStatus, HandoffRecord,
    LeaderLeaseRecord, MirrorOutboxRecord, MirrorStatus, PromptId, RecoveryCapsule,
    RecoveryCapsuleOutcome, ReservationConflict, ReservationMode, ReservationRecord, RunId,
    RunStatus, SessionId, SessionStatus, StopReason, TaskRunRecord, Timestamp,
};

mod archive;
mod ops;
mod playbook;

use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Transaction};
use serde::Serialize;
use serde_json::Value;

pub const CRATE_PURPOSE: &str = "SQLite bootstrap, migrations, and runtime persistence.";

#[derive(Debug, Clone, Copy)]
pub struct ReservationRequest<'a> {
    pub path_pattern: &'a str,
    pub mode: ReservationMode,
    pub reason: Option<&'a str>,
    pub expires_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct ReservationAcquireOutcome {
    pub acquired: Vec<ReservationRecord>,
    pub conflicts: Vec<ReservationConflict>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryReason {
    RunNoLongerActive,
    ActiveRunInterrupted,
}

#[derive(Debug, Clone)]
pub struct RecoveredReservation {
    pub reservation: ReservationRecord,
    pub reason: RecoveryReason,
}

#[derive(Debug, Clone)]
pub struct RunStartInput {
    pub run_id: RunId,
    pub bead_id: BeadId,
    pub attempt_no: i32,
    pub started_at: chrono::DateTime<Utc>,
    pub escalation_tier: grove_types::EscalationTier,
}

#[derive(Debug, Clone)]
pub struct RunFinishInput {
    pub run_id: RunId,
    pub status: RunStatus,
    pub failure_class: Option<FailureClass>,
    pub failure_detail: Option<String>,
    pub ended_at: chrono::DateTime<Utc>,
    pub retry_after: Option<chrono::DateTime<Utc>>,
    pub circuit_breaker_state: Option<CircuitBreakerState>,
}

#[derive(Debug, Clone)]
pub struct SessionCheckpointInput {
    pub checkpoint_id: CheckpointId,
    pub bead_id: BeadId,
    pub run_id: RunId,
    pub session_id: SessionId,
    pub payload: CheckpointPayload,
    pub saved_at: chrono::DateTime<Utc>,
    pub resume_generation: u32,
}

#[derive(Debug, Clone)]
pub struct HandoffWriteInput {
    pub bead_id: BeadId,
    pub run_id: RunId,
    pub summary: String,
    pub artifacts: Vec<String>,
    pub lessons: Vec<String>,
    pub decisions: Vec<String>,
    pub warnings: Vec<String>,
    pub completed_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct EventLogInput {
    pub kind: EventKind,
    pub bead_id: Option<BeadId>,
    pub run_id: Option<RunId>,
    pub session_id: Option<SessionId>,
    pub payload: Value,
    pub created_at: chrono::DateTime<Utc>,
    pub observability: EventObservability,
}

#[derive(Debug, Clone)]
pub struct LeaderLeaseAcquireInput {
    pub owner_label: String,
    pub run_id: Option<RunId>,
    pub acquired_at: chrono::DateTime<Utc>,
    pub expires_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct InterruptedRunRecovery {
    pub run: TaskRunRecord,
    pub bead_id: BeadId,
    pub recovery_capsule: Option<RecoveryCapsule>,
}

#[derive(Debug, Clone)]
pub struct MirrorOutboxWriteInput {
    pub bead_id: BeadId,
    pub run_id: RunId,
    pub handoff: HandoffRecord,
    pub close_bead: bool,
}

#[derive(Debug, Clone)]
pub struct MirrorOutboxUpdateInput {
    pub id: String,
    pub mirror_status: MirrorStatus,
    pub last_attempt_at: Option<chrono::DateTime<Utc>>,
    pub next_retry_after: Option<chrono::DateTime<Utc>>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecoveryCapsuleEvent {
    pub capsule: RecoveryCapsule,
    pub source_event_id: i64,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone)]
pub struct RecoveryCapsuleWriteInput {
    pub bead_id: BeadId,
    pub run_id: RunId,
    pub capsule: RecoveryCapsule,
    pub created_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Clone, Default)]
pub struct EventObservability {
    pub correlation_id: Option<String>,
    pub operation: Option<String>,
    pub outcome: Option<EventOutcome>,
    pub duration_ms: Option<u64>,
    pub error: Option<EventError>,
    pub context_snapshot: Option<ContextSnapshot>,
}

const PRAGMAS: &[&str] = &[
    "PRAGMA journal_mode = WAL;",
    "PRAGMA foreign_keys = ON;",
    "PRAGMA synchronous = NORMAL;",
    "PRAGMA temp_store = MEMORY;",
    "PRAGMA busy_timeout = 5000;",
];

const MIGRATION_MANIFEST: &[Migration<'_>] = &[
    Migration {
        version: 1,
        name: "0001_init.sql",
        sql: include_str!("../migrations/0001_init.sql"),
    },
    Migration {
        version: 2,
        name: "0002_prompt_manifest_columns.sql",
        sql: include_str!("../migrations/0002_prompt_manifest_columns.sql"),
    },
    Migration {
        version: 3,
        name: "0003_leader_lease.sql",
        sql: include_str!("../migrations/0003_leader_lease.sql"),
    },
    Migration {
        version: 4,
        name: "0004_mirror_outbox.sql",
        sql: include_str!("../migrations/0004_mirror_outbox.sql"),
    },
    Migration {
        version: 5,
        name: "0005_operational_schema.sql",
        sql: include_str!("../migrations/0005_operational_schema.sql"),
    },
    Migration {
        version: 6,
        name: "0006_observability.sql",
        sql: include_str!("../migrations/0006_observability.sql"),
    },
    Migration {
        version: 7,
        name: "0007_archive_fts.sql",
        sql: include_str!("../migrations/0007_archive_fts.sql"),
    },
    Migration {
        version: 8,
        name: "0008_archive_watermarks.sql",
        sql: include_str!("../migrations/0008_archive_watermarks.sql"),
    },
    Migration {
        version: 9,
        name: "0009_playbook.sql",
        sql: include_str!("../migrations/0009_playbook.sql"),
    },
    Migration {
        version: 10,
        name: "0010_activity_state.sql",
        sql: include_str!("../migrations/0010_activity_state.sql"),
    },
    Migration {
        version: 11,
        name: "0011_circuit_breaker_state.sql",
        sql: include_str!("../migrations/0011_circuit_breaker_state.sql"),
    },
];

#[derive(Debug)]
pub struct Database {
    conn: Connection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationState {
    pub version: i64,
    pub name: String,
}

#[derive(Debug, Clone, Copy)]
struct Migration<'a> {
    version: i64,
    name: &'a str,
    sql: &'a str,
}

#[derive(Debug)]
struct RawBeadRecordRow {
    bead_id: String,
    title: String,
    description: Option<String>,
    priority: i64,
    issue_type: String,
    br_status: String,
    assignee: Option<String>,
    labels_json: String,
    raw_json: String,
    synced_at: String,
    grove_status: Option<String>,
    declared_paths_json: Option<String>,
    metadata_json: Option<String>,
    last_run_id: Option<String>,
    retry_after: Option<String>,
    last_failure_class: Option<String>,
    last_failure_detail: Option<String>,
    circuit_breaker_json: Option<String>,
    runtime_updated_at: Option<String>,
}

#[derive(Debug)]
struct RawTaskRunRow {
    id: String,
    bead_id: String,
    attempt_no: i32,
    status: String,
    failure_class: Option<String>,
    failure_detail: Option<String>,
    started_at: String,
    ended_at: Option<String>,
    session_count: i32,
    checkpoint_count: i32,
    last_checkpoint_id: Option<String>,
    activity: Option<String>,
    last_activity_at: Option<String>,
    escalation_tier: String,
}

#[derive(Debug)]
struct RawSessionRow {
    id: String,
    run_id: String,
    external_session_id: Option<String>,
    ordinal_in_run: i32,
    status: String,
    started_at: String,
    ended_at: Option<String>,
    prompt_id: Option<String>,
    prompt_manifest_path: Option<String>,
    prompt_bytes: i32,
    estimated_input_tokens: i32,
    estimated_output_tokens: i32,
    exit_code: Option<i32>,
    stop_reason: Option<String>,
    transcript_path: String,
}

#[derive(Debug)]
struct RawCheckpointRow {
    id: String,
    bead_id: String,
    run_id: String,
    session_id: String,
    progress: String,
    next_step: String,
    payload_json: String,
    saved_at: String,
    resume_generation: u32,
}

#[derive(Debug)]
struct RawHandoffRow {
    bead_id: String,
    run_id: String,
    summary: String,
    artifacts_json: String,
    lessons_json: String,
    decisions_json: String,
    warnings_json: String,
    completed_at: String,
}

#[derive(Debug)]
struct RawEventLogRow {
    id: i64,
    kind: String,
    bead_id: Option<String>,
    run_id: Option<String>,
    session_id: Option<String>,
    payload_json: String,
    created_at: String,
    correlation_id: Option<String>,
    operation: Option<String>,
    outcome: Option<String>,
    duration_ms: Option<i64>,
    error_json: Option<String>,
    context_snapshot_json: Option<String>,
}

#[derive(Debug)]
struct RawReservationRow {
    id: i64,
    bead_id: String,
    run_id: Option<String>,
    path_pattern: String,
    exclusive: bool,
    reason: Option<String>,
    expires_at: String,
    released_at: Option<String>,
}

#[derive(Debug)]
struct RawLeaderLeaseRow {
    owner_label: String,
    run_id: Option<String>,
    acquired_at: String,
    heartbeat_at: String,
    expires_at: String,
    released_at: Option<String>,
}

impl Database {
    pub fn open(path: &Utf8Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create database parent directory: {parent}"))?;
        }

        let connection = Connection::open_with_flags(
            utf8_to_std_path(path)?,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )
        .with_context(|| format!("open SQLite database at {path}"))?;

        apply_pragmas(&connection)?;

        Ok(Self { conn: connection })
    }

    pub fn migrate(&mut self) -> Result<()> {
        ensure_migration_table(&self.conn)?;

        for migration in MIGRATION_MANIFEST {
            let applied_name = self.applied_migration_name(migration.version)?;
            match applied_name {
                Some(existing_name) if existing_name == migration.name => continue,
                Some(existing_name) => {
                    bail!(
                        "migration version {} already applied with different name: {} != {}",
                        migration.version,
                        existing_name,
                        migration.name
                    );
                }
                None => self.apply_migration(*migration)?,
            }
        }

        Ok(())
    }

    pub fn with_tx<T>(&mut self, f: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        let tx = self.conn.transaction().context("begin transaction")?;
        let value = f(&tx)?;
        tx.commit().context("commit transaction")?;
        Ok(value)
    }

    #[must_use]
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    pub fn applied_migrations(&self) -> Result<Vec<MigrationState>> {
        let mut stmt = self
            .conn
            .prepare("SELECT version, name FROM _migrations ORDER BY version")
            .context("prepare applied migrations query")?;

        let rows = stmt
            .query_map([], |row| {
                let version = row.get(0)?;
                let name: String = row.get(1)?;
                Ok((version, name))
            })
            .context("query applied migrations")?;

        let pairs = rows
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect applied migrations")?;

        Ok(pairs
            .into_iter()
            .map(|(version, name)| MigrationState { version, name })
            .collect())
    }

    pub fn list_bead_records(&self) -> Result<Vec<GroveBeadRecord>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT c.bead_id, c.title, c.description, c.priority, c.issue_type, c.status, c.assignee, \
                    c.labels_json, c.raw_json, c.synced_at, r.grove_status, r.declared_paths_json, \
                    r.metadata_json, r.last_run_id, r.retry_after, r.last_failure_class, \
                    r.last_failure_detail, r.circuit_breaker_json, r.runtime_updated_at \
                 FROM bead_cache c \
                 LEFT JOIN bead_runtime r ON r.bead_id = c.bead_id \
                 ORDER BY c.priority ASC, c.bead_id ASC",
            )
            .context("prepare bead record list query")?;

        let rows = stmt
            .query_map([], raw_bead_record_row)
            .context("query bead records")?;

        let raw_rows = rows
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect bead records")?;

        raw_rows
            .into_iter()
            .map(raw_bead_record_into_record)
            .collect()
    }

    pub fn get_bead_record(&self, bead_id: &BeadId) -> Result<Option<GroveBeadRecord>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT c.bead_id, c.title, c.description, c.priority, c.issue_type, c.status, c.assignee, \
                    c.labels_json, c.raw_json, c.synced_at, r.grove_status, r.declared_paths_json, \
                    r.metadata_json, r.last_run_id, r.retry_after, r.last_failure_class, \
                    r.last_failure_detail, r.circuit_breaker_json, r.runtime_updated_at \
                 FROM bead_cache c \
                 LEFT JOIN bead_runtime r ON r.bead_id = c.bead_id \
                 WHERE c.bead_id = ?1",
            )
            .context("prepare single bead record query")?;

        let raw = stmt
            .query_row([bead_id.as_str()], raw_bead_record_row)
            .optional()
            .with_context(|| format!("query bead record for {}", bead_id.as_str()))?;

        raw.map(raw_bead_record_into_record).transpose()
    }

    pub fn dependency_snapshot(&self, bead_id: &BeadId) -> Result<BrDependencySnapshot> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT parent_id, child_id \
                 FROM bead_dependencies \
                 WHERE relation_type = 'blocks' \
                   AND (parent_id = ?1 OR child_id = ?1) \
                 ORDER BY parent_id, child_id",
            )
            .context("prepare dependency snapshot query")?;

        let rows = stmt
            .query_map([bead_id.as_str()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .with_context(|| format!("query dependency snapshot for {}", bead_id.as_str()))?;

        let edges = rows
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect dependency snapshot rows")?;

        let mut blocked_by = Vec::new();
        let mut blocks = Vec::new();

        for (parent_id, child_id) in edges {
            if child_id == bead_id.as_str() {
                blocked_by.push(BeadId::new(parent_id.clone()));
            }
            if parent_id == bead_id.as_str() {
                blocks.push(BeadId::new(child_id));
            }
        }

        Ok(BrDependencySnapshot {
            bead_id: bead_id.clone(),
            blocked_by,
            blocks,
            rows: Vec::new(),
        })
    }

    pub fn list_task_runs_for_bead(&self, bead_id: &BeadId) -> Result<Vec<TaskRunRecord>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, bead_id, attempt_no, status, failure_class, failure_detail, started_at, ended_at, \
                    session_count, checkpoint_count, last_checkpoint_id, activity, last_activity_at, escalation_tier \
                 FROM task_runs \
                 WHERE bead_id = ?1 \
                 ORDER BY attempt_no DESC, started_at DESC",
            )
            .context("prepare task run list query")?;

        let rows = stmt
            .query_map([bead_id.as_str()], raw_task_run_row)
            .with_context(|| format!("query task runs for {}", bead_id.as_str()))?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("collect task run rows")?
            .into_iter()
            .map(raw_task_run_into_record)
            .collect()
    }

    pub fn record_run_started(&mut self, input: RunStartInput) -> Result<TaskRunRecord> {
        let tx = self
            .conn
            .transaction()
            .context("begin run start transaction")?;
        ensure_bead_exists(&tx, &input.bead_id)?;
        tx.execute(
            "INSERT INTO task_runs(\
                id, bead_id, attempt_no, status, failure_class, failure_detail, started_at, ended_at, session_count, checkpoint_count, last_checkpoint_id, activity, last_activity_at, escalation_tier\
             ) VALUES (?1, ?2, ?3, ?4, NULL, NULL, ?5, NULL, 0, 0, NULL, ?6, ?7, ?8)",
            params![
                input.run_id.as_str(),
                input.bead_id.as_str(),
                input.attempt_no,
                encode_run_status(RunStatus::Active),
                timestamp_string(&input.started_at),
                encode_agent_activity(grove_types::AgentActivity::Active),
                timestamp_string(&input.started_at),
                encode_escalation_tier(input.escalation_tier),
            ],
        )
        .with_context(|| format!("insert task run {}", input.run_id.as_str()))?;
        upsert_bead_runtime_tx(
            &tx,
            &input.bead_id,
            Some(GroveBeadStatus::Running),
            None,
            Some(Some(input.run_id.clone())),
            Some(None),
            Some(None),
            Some(None),
            None,
            &input.started_at,
        )?;
        insert_event_log_tx(
            &tx,
            EventKind::RunStarted,
            Some(&input.bead_id),
            Some(&input.run_id),
            None,
            &serde_json::json!({
                "attempt_no": input.attempt_no,
                "status": encode_run_status(RunStatus::Active),
            }),
            &input.started_at,
        )?;
        let raw = tx
            .query_row(
                "SELECT id, bead_id, attempt_no, status, failure_class, failure_detail, started_at, ended_at, session_count, checkpoint_count, last_checkpoint_id, activity, last_activity_at, escalation_tier \
                 FROM task_runs WHERE id = ?1",
                [input.run_id.as_str()],
                raw_task_run_row,
            )
            .with_context(|| format!("query inserted task run {}", input.run_id.as_str()))?;
        tx.commit().context("commit run start transaction")?;
        raw_task_run_into_record(raw)
    }

    pub fn record_session_started(
        &mut self,
        bead_id: &BeadId,
        session: &ClaudeSessionRecord,
    ) -> Result<ClaudeSessionRecord> {
        let tx = self
            .conn
            .transaction()
            .context("begin session start transaction")?;
        ensure_bead_exists(&tx, bead_id)?;
        ensure_run_exists(&tx, &session.run_id)?;
        ensure_run_belongs_to_bead(&tx, &session.run_id, bead_id)?;
        tx.execute(
            "INSERT INTO claude_sessions(\
                id, run_id, external_session_id, ordinal_in_run, status, started_at, ended_at, prompt_id, prompt_manifest_path, prompt_bytes, estimated_input_tokens, estimated_output_tokens, exit_code, stop_reason, transcript_path\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                session.id.as_str(),
                session.run_id.as_str(),
                session.external_session_id.as_deref(),
                session.ordinal_in_run,
                encode_session_status(session.status),
                timestamp_string(&session.started_at),
                session.ended_at.as_ref().map(timestamp_string),
                session.prompt_id.as_ref().map(PromptId::as_str),
                session.prompt_manifest_path.as_deref(),
                session.prompt_bytes,
                session.estimated_input_tokens,
                session.estimated_output_tokens,
                session.exit_code,
                session.stop_reason.map(encode_stop_reason),
                session.transcript_path.as_str(),
            ],
        )
        .with_context(|| format!("insert session {}", session.id.as_str()))?;
        tx.execute(
            "UPDATE task_runs SET session_count = session_count + 1 WHERE id = ?1",
            [session.run_id.as_str()],
        )
        .with_context(|| format!("increment session count for {}", session.run_id.as_str()))?;
        upsert_bead_runtime_tx(
            &tx,
            bead_id,
            Some(GroveBeadStatus::Running),
            None,
            Some(Some(session.run_id.clone())),
            None,
            Some(None),
            Some(None),
            None,
            &session.started_at,
        )?;
        insert_event_log_tx(
            &tx,
            EventKind::SessionStarted,
            Some(bead_id),
            Some(&session.run_id),
            Some(&session.id),
            &serde_json::json!({
                "ordinal_in_run": session.ordinal_in_run,
                "status": encode_session_status(session.status),
            }),
            &session.started_at,
        )?;
        tx.commit().context("commit session start transaction")?;
        Ok(session.clone())
    }

    pub fn record_session_finished(
        &mut self,
        bead_id: &BeadId,
        session: &ClaudeSessionRecord,
    ) -> Result<ClaudeSessionRecord> {
        let tx = self
            .conn
            .transaction()
            .context("begin session finish transaction")?;
        ensure_bead_exists(&tx, bead_id)?;
        ensure_run_exists(&tx, &session.run_id)?;
        ensure_run_belongs_to_bead(&tx, &session.run_id, bead_id)?;
        ensure_session_belongs_to_run(&tx, &session.id, &session.run_id)?;
        tx.execute(
            "UPDATE claude_sessions SET \
                external_session_id = ?2, ordinal_in_run = ?3, status = ?4, started_at = ?5, ended_at = ?6, prompt_id = ?7, prompt_manifest_path = ?8, prompt_bytes = ?9, estimated_input_tokens = ?10, estimated_output_tokens = ?11, exit_code = ?12, stop_reason = ?13, transcript_path = ?14 \
             WHERE id = ?1",
            params![
                session.id.as_str(),
                session.external_session_id.as_deref(),
                session.ordinal_in_run,
                encode_session_status(session.status),
                timestamp_string(&session.started_at),
                session.ended_at.as_ref().map(timestamp_string),
                session.prompt_id.as_ref().map(PromptId::as_str),
                session.prompt_manifest_path.as_deref(),
                session.prompt_bytes,
                session.estimated_input_tokens,
                session.estimated_output_tokens,
                session.exit_code,
                session.stop_reason.map(encode_stop_reason),
                session.transcript_path.as_str(),
            ],
        )
        .with_context(|| format!("update session {}", session.id.as_str()))?;
        let event_kind = match session.status {
            SessionStatus::Checkpointed => EventKind::SessionCheckpointed,
            SessionStatus::Completed => EventKind::SessionSucceeded,
            SessionStatus::TimedOut
            | SessionStatus::RateLimited
            | SessionStatus::PermissionDenied
            | SessionStatus::Crashed
            | SessionStatus::UnknownFailure => EventKind::SessionFailed,
            SessionStatus::Starting | SessionStatus::Running => EventKind::SessionStarted,
        };
        let runtime_status = match session.status {
            SessionStatus::Checkpointed => GroveBeadStatus::Checkpointed,
            SessionStatus::Completed => GroveBeadStatus::Succeeded,
            SessionStatus::TimedOut | SessionStatus::RateLimited => GroveBeadStatus::WaitingToRetry,
            SessionStatus::PermissionDenied
            | SessionStatus::Crashed
            | SessionStatus::UnknownFailure => GroveBeadStatus::Failed,
            SessionStatus::Starting | SessionStatus::Running => GroveBeadStatus::Running,
        };
        let failure_class = session_failure_class(session);
        let failure_detail = session
            .stop_reason
            .map(|reason| format!("session ended with {:?}", reason));
        upsert_bead_runtime_tx(
            &tx,
            bead_id,
            Some(runtime_status),
            None,
            Some(Some(session.run_id.clone())),
            Some(None),
            Some(failure_class),
            Some(failure_detail.clone()),
            None,
            &session.ended_at.unwrap_or_else(Utc::now),
        )?;
        insert_event_log_tx(
            &tx,
            event_kind,
            Some(bead_id),
            Some(&session.run_id),
            Some(&session.id),
            &serde_json::json!({
                "status": encode_session_status(session.status),
                "stop_reason": session.stop_reason.map(encode_stop_reason),
                "exit_code": session.exit_code,
            }),
            &session.ended_at.unwrap_or_else(Utc::now),
        )?;
        tx.commit().context("commit session finish transaction")?;
        Ok(session.clone())
    }

    pub fn record_checkpoint_saved(
        &mut self,
        input: SessionCheckpointInput,
    ) -> Result<CheckpointRecord> {
        let tx = self
            .conn
            .transaction()
            .context("begin checkpoint save transaction")?;
        ensure_bead_exists(&tx, &input.bead_id)?;
        ensure_run_exists(&tx, &input.run_id)?;
        ensure_run_belongs_to_bead(&tx, &input.run_id, &input.bead_id)?;
        ensure_session_belongs_to_run(&tx, &input.session_id, &input.run_id)?;
        tx.execute(
            "INSERT INTO checkpoints(\
                id, bead_id, run_id, session_id, progress, next_step, payload_json, saved_at, resume_generation\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                input.checkpoint_id.as_str(),
                input.bead_id.as_str(),
                input.run_id.as_str(),
                input.session_id.as_str(),
                input.payload.progress,
                input.payload.next_step,
                serde_json::to_string(&input.payload).context("serialize checkpoint payload")?,
                timestamp_string(&input.saved_at),
                input.resume_generation,
            ],
        )
        .with_context(|| format!("insert checkpoint {}", input.checkpoint_id.as_str()))?;
        tx.execute(
            "UPDATE task_runs SET checkpoint_count = checkpoint_count + 1, last_checkpoint_id = ?2 WHERE id = ?1",
            params![input.run_id.as_str(), input.checkpoint_id.as_str()],
        )
        .with_context(|| format!("update checkpoint counters for {}", input.run_id.as_str()))?;
        upsert_bead_runtime_tx(
            &tx,
            &input.bead_id,
            Some(GroveBeadStatus::Checkpointed),
            Some(input.payload.claimed_paths.clone()),
            Some(Some(input.run_id.clone())),
            Some(None),
            Some(None),
            Some(None),
            None,
            &input.saved_at,
        )?;
        insert_event_log_tx(
            &tx,
            EventKind::SessionCheckpointed,
            Some(&input.bead_id),
            Some(&input.run_id),
            Some(&input.session_id),
            &serde_json::json!({
                "checkpoint_id": input.checkpoint_id.as_str(),
                "resume_generation": input.resume_generation,
                "next_step": input.payload.next_step,
            }),
            &input.saved_at,
        )?;
        let raw = tx
            .query_row(
                "SELECT id, bead_id, run_id, session_id, progress, next_step, payload_json, saved_at, resume_generation \
                 FROM checkpoints WHERE id = ?1",
                [input.checkpoint_id.as_str()],
                raw_checkpoint_row,
            )
            .with_context(|| format!("query inserted checkpoint {}", input.checkpoint_id.as_str()))?;
        tx.commit().context("commit checkpoint save transaction")?;
        raw_checkpoint_into_record(raw)
    }

    pub fn record_run_finished(
        &mut self,
        bead_id: &BeadId,
        input: RunFinishInput,
    ) -> Result<TaskRunRecord> {
        let tx = self
            .conn
            .transaction()
            .context("begin run finish transaction")?;
        ensure_bead_exists(&tx, bead_id)?;
        ensure_run_exists(&tx, &input.run_id)?;
        ensure_run_belongs_to_bead(&tx, &input.run_id, bead_id)?;
        tx.execute(
            "UPDATE task_runs SET status = ?2, failure_class = ?3, failure_detail = ?4, ended_at = ?5, last_activity_at = ?6 WHERE id = ?1",
            params![
                input.run_id.as_str(),
                encode_run_status(input.status),
                input.failure_class.map(encode_failure_class),
                input.failure_detail.as_deref(),
                timestamp_string(&input.ended_at),
                timestamp_string(&input.ended_at),
            ],
        )
        .with_context(|| format!("update task run {}", input.run_id.as_str()))?;
        let runtime_status = match input.status {
            RunStatus::Active => GroveBeadStatus::Running,
            RunStatus::Checkpointed => GroveBeadStatus::Checkpointed,
            RunStatus::WaitingToRetry => GroveBeadStatus::WaitingToRetry,
            RunStatus::Succeeded => GroveBeadStatus::Succeeded,
            RunStatus::Failed => GroveBeadStatus::Failed,
        };
        let declared_paths = match input.status {
            RunStatus::Checkpointed => None,
            _ => Some(Vec::new()),
        };
        upsert_bead_runtime_tx(
            &tx,
            bead_id,
            Some(runtime_status),
            declared_paths,
            Some(Some(input.run_id.clone())),
            Some(input.retry_after),
            Some(input.failure_class),
            Some(input.failure_detail.clone()),
            Some(input.circuit_breaker_state.clone()),
            &input.ended_at,
        )?;
        let event_kind = match input.status {
            RunStatus::Active => EventKind::RunStarted,
            RunStatus::Checkpointed => EventKind::RunCheckpointed,
            RunStatus::Succeeded => EventKind::RunSucceeded,
            RunStatus::WaitingToRetry | RunStatus::Failed => EventKind::RunFailed,
        };
        insert_event_log_tx(
            &tx,
            event_kind,
            Some(bead_id),
            Some(&input.run_id),
            None,
            &serde_json::json!({
                "status": encode_run_status(input.status),
                "failure_class": input.failure_class.map(encode_failure_class),
                "failure_detail": input.failure_detail,
                "retry_after": input.retry_after.map(|ts| timestamp_string(&ts)),
            }),
            &input.ended_at,
        )?;
        let raw = tx
            .query_row(
                "SELECT id, bead_id, attempt_no, status, failure_class, failure_detail, started_at, ended_at, session_count, checkpoint_count, last_checkpoint_id, activity, last_activity_at, escalation_tier \
                 FROM task_runs WHERE id = ?1",
                [input.run_id.as_str()],
                raw_task_run_row,
            )
            .with_context(|| format!("query updated task run {}", input.run_id.as_str()))?;
        tx.commit().context("commit run finish transaction")?;
        raw_task_run_into_record(raw)
    }

    pub fn latest_session_for_run(&self, run_id: &RunId) -> Result<Option<ClaudeSessionRecord>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, run_id, external_session_id, ordinal_in_run, status, started_at, ended_at, \
                    prompt_id, prompt_manifest_path, prompt_bytes, estimated_input_tokens, estimated_output_tokens, exit_code, stop_reason, transcript_path \
                 FROM claude_sessions \
                 WHERE run_id = ?1 \
                 ORDER BY ordinal_in_run DESC, started_at DESC \
                 LIMIT 1",
            )
            .context("prepare latest session query")?;

        let raw = stmt
            .query_row([run_id.as_str()], raw_session_row)
            .optional()
            .with_context(|| format!("query latest session for {}", run_id.as_str()))?;

        raw.map(raw_session_into_record).transpose()
    }

    pub fn latest_checkpoint_for_bead(&self, bead_id: &BeadId) -> Result<Option<CheckpointRecord>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, bead_id, run_id, session_id, progress, next_step, payload_json, saved_at, resume_generation \
                 FROM checkpoints \
                 WHERE bead_id = ?1 \
                 ORDER BY saved_at DESC, id DESC \
                 LIMIT 1",
            )
            .context("prepare latest checkpoint query")?;

        let raw = stmt
            .query_row([bead_id.as_str()], raw_checkpoint_row)
            .optional()
            .with_context(|| format!("query latest checkpoint for {}", bead_id.as_str()))?;

        raw.map(raw_checkpoint_into_record).transpose()
    }

    pub fn handoff_for_bead(&self, bead_id: &BeadId) -> Result<Option<HandoffRecord>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT bead_id, run_id, summary, artifacts_json, lessons_json, decisions_json, warnings_json, completed_at \
                 FROM handoffs \
                 WHERE bead_id = ?1 \
                 ORDER BY completed_at DESC, run_id DESC \
                 LIMIT 1",
            )
            .context("prepare handoff query")?;

        let raw = stmt
            .query_row([bead_id.as_str()], raw_handoff_row)
            .optional()
            .with_context(|| format!("query handoff for {}", bead_id.as_str()))?;

        raw.map(raw_handoff_into_record).transpose()
    }

    pub fn write_handoff(&mut self, input: HandoffWriteInput) -> Result<HandoffRecord> {
        let tx = self
            .conn
            .transaction()
            .context("begin handoff write transaction")?;
        ensure_bead_exists(&tx, &input.bead_id)?;
        ensure_run_exists(&tx, &input.run_id)?;
        ensure_run_belongs_to_bead(&tx, &input.run_id, &input.bead_id)?;

        tx.execute(
            "INSERT INTO handoffs(\
                bead_id, run_id, summary, artifacts_json, lessons_json, decisions_json, warnings_json, completed_at\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                input.bead_id.as_str(),
                input.run_id.as_str(),
                &input.summary,
                serde_json::to_string(&input.artifacts).context("serialize handoff artifacts")?,
                serde_json::to_string(&input.lessons).context("serialize handoff lessons")?,
                serde_json::to_string(&input.decisions).context("serialize handoff decisions")?,
                serde_json::to_string(&input.warnings).context("serialize handoff warnings")?,
                timestamp_string(&input.completed_at),
            ],
        )
        .with_context(|| format!("insert handoff for {}", input.bead_id.as_str()))?;

        insert_event_log_tx(
            &tx,
            EventKind::HandoffWritten,
            Some(&input.bead_id),
            Some(&input.run_id),
            None,
            &serde_json::json!({
                "summary": input.summary,
                "artifacts": input.artifacts,
                "lessons": input.lessons,
                "decisions": input.decisions,
                "warnings": input.warnings,
            }),
            &input.completed_at,
        )?;

        let raw = tx
            .query_row(
                "SELECT bead_id, run_id, summary, artifacts_json, lessons_json, decisions_json, warnings_json, completed_at \
                 FROM handoffs \
                 WHERE bead_id = ?1 AND run_id = ?2",
                params![input.bead_id.as_str(), input.run_id.as_str()],
                raw_handoff_row,
            )
            .with_context(|| format!("query inserted handoff for {}", input.bead_id.as_str()))?;
        tx.commit().context("commit handoff write transaction")?;
        raw_handoff_into_record(raw)
    }

    pub fn parent_handoffs_for_bead(&self, bead_id: &BeadId) -> Result<Vec<HandoffRecord>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT h.bead_id, h.run_id, h.summary, h.artifacts_json, h.lessons_json, h.decisions_json, h.warnings_json, h.completed_at \
                 FROM bead_dependencies d \
                 JOIN handoffs h ON h.bead_id = d.parent_id \
                 WHERE d.relation_type = 'blocks' AND d.child_id = ?1 \
                   AND h.completed_at = (\
                       SELECT MAX(h2.completed_at) FROM handoffs h2 WHERE h2.bead_id = d.parent_id\
                   ) \
                 ORDER BY h.completed_at ASC, h.bead_id ASC",
            )
            .context("prepare parent handoffs query")?;

        let rows = stmt
            .query_map([bead_id.as_str()], raw_handoff_row)
            .with_context(|| format!("query parent handoffs for {}", bead_id.as_str()))?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("collect parent handoff rows")?
            .into_iter()
            .map(raw_handoff_into_record)
            .collect()
    }

    pub fn active_leader_lease(
        &self,
        now: &chrono::DateTime<Utc>,
    ) -> Result<Option<LeaderLeaseRecord>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT owner_label, run_id, acquired_at, heartbeat_at, expires_at, released_at \
                 FROM leader_leases \
                 WHERE slot = 1 AND released_at IS NULL AND expires_at > ?1",
            )
            .context("prepare active leader lease query")?;

        let raw = stmt
            .query_row([timestamp_string(now)], raw_leader_lease_row)
            .optional()
            .context("query active leader lease")?;

        raw.map(raw_leader_lease_into_record).transpose()
    }

    pub fn acquire_leader_lease(
        &mut self,
        input: LeaderLeaseAcquireInput,
    ) -> Result<Option<LeaderLeaseRecord>> {
        let tx = self
            .conn
            .transaction()
            .context("begin leader lease acquire transaction")?;
        if let Some(run_id) = input.run_id.as_ref() {
            ensure_run_exists(&tx, run_id)?;
        }

        let current = active_leader_lease_tx(&tx, &input.acquired_at)?;
        if current.is_some() {
            tx.commit()
                .context("commit contested leader lease transaction")?;
            return Ok(None);
        }

        tx.execute("DELETE FROM leader_leases WHERE slot = 1", [])
            .context("clear prior leader lease row")?;

        tx.execute(
            "INSERT INTO leader_leases(\
                slot, owner_label, run_id, acquired_at, heartbeat_at, expires_at, released_at\
             ) VALUES (1, ?1, ?2, ?3, ?4, ?5, NULL)",
            params![
                input.owner_label,
                input.run_id.as_ref().map(RunId::as_str),
                timestamp_string(&input.acquired_at),
                timestamp_string(&input.acquired_at),
                timestamp_string(&input.expires_at),
            ],
        )
        .context("insert leader lease")?;

        insert_event_log_tx(
            &tx,
            EventKind::LeaseAcquired,
            None,
            input.run_id.as_ref(),
            None,
            &serde_json::json!({
                "owner_label": input.owner_label,
                "acquired_at": timestamp_string(&input.acquired_at),
                "expires_at": timestamp_string(&input.expires_at),
            }),
            &input.acquired_at,
        )?;

        let lease = active_leader_lease_tx(&tx, &input.acquired_at)?
            .context("leader lease should exist after acquire")?;
        tx.commit()
            .context("commit leader lease acquire transaction")?;
        Ok(Some(lease))
    }

    pub fn heartbeat_leader_lease(
        &mut self,
        owner_label: &str,
        now: &chrono::DateTime<Utc>,
        expires_at: &chrono::DateTime<Utc>,
    ) -> Result<Option<LeaderLeaseRecord>> {
        let tx = self
            .conn
            .transaction()
            .context("begin leader lease heartbeat transaction")?;
        let updated = tx
            .execute(
                "UPDATE leader_leases \
             SET heartbeat_at = ?1, expires_at = ?2 \
             WHERE slot = 1 AND released_at IS NULL AND owner_label = ?3 AND expires_at > ?1",
                params![
                    timestamp_string(now),
                    timestamp_string(expires_at),
                    owner_label
                ],
            )
            .context("update leader lease heartbeat")?;
        if updated == 0 {
            tx.commit()
                .context("commit empty leader lease heartbeat transaction")?;
            return Ok(None);
        }

        insert_event_log_tx(
            &tx,
            EventKind::LeaseHeartbeat,
            None,
            None,
            None,
            &serde_json::json!({
                "owner_label": owner_label,
                "heartbeat_at": timestamp_string(now),
                "expires_at": timestamp_string(expires_at),
            }),
            now,
        )?;

        let lease = active_leader_lease_tx(&tx, now)?
            .context("leader lease should exist after heartbeat")?;
        tx.commit()
            .context("commit leader lease heartbeat transaction")?;
        Ok(Some(lease))
    }

    pub fn release_leader_lease(
        &mut self,
        owner_label: &str,
        released_at: &chrono::DateTime<Utc>,
    ) -> Result<Option<LeaderLeaseRecord>> {
        let tx = self
            .conn
            .transaction()
            .context("begin leader lease release transaction")?;
        let lease = active_leader_lease_tx(&tx, released_at)?;
        let Some(lease) = lease else {
            tx.commit()
                .context("commit empty leader lease release transaction")?;
            return Ok(None);
        };
        if lease.owner_label != owner_label {
            tx.commit()
                .context("commit mismatched leader lease release transaction")?;
            return Ok(None);
        }

        tx.execute(
            "UPDATE leader_leases SET released_at = ?1 WHERE slot = 1 AND released_at IS NULL",
            [timestamp_string(released_at)],
        )
        .context("release leader lease")?;

        insert_event_log_tx(
            &tx,
            EventKind::LeaseReleased,
            None,
            lease.run_id.as_ref(),
            None,
            &serde_json::json!({
                "owner_label": owner_label,
                "released_at": timestamp_string(released_at),
            }),
            released_at,
        )?;

        tx.commit()
            .context("commit leader lease release transaction")?;
        Ok(Some(lease))
    }

    pub fn reconcile_interrupted_runs(
        &mut self,
        now: &chrono::DateTime<Utc>,
    ) -> Result<Vec<InterruptedRunRecovery>> {
        let tx = self
            .conn
            .transaction()
            .context("begin interrupted run reconciliation transaction")?;
        let active_runs = list_runs_by_status_tx(&tx, RunStatus::Active)?;
        let mut recovered = Vec::new();

        for run in active_runs {
            let failure_detail =
                "startup reconciliation marked previously active run as interrupted";
            tx.execute(
                "UPDATE task_runs SET status = ?2, failure_class = ?3, failure_detail = ?4, ended_at = ?5 WHERE id = ?1",
                params![
                    run.id.as_str(),
                    encode_run_status(RunStatus::Failed),
                    encode_failure_class(FailureClass::Interrupted),
                    failure_detail,
                    timestamp_string(now),
                ],
            )
            .with_context(|| format!("mark interrupted run {}", run.id.as_str()))?;

            upsert_bead_runtime_tx(
                &tx,
                &run.bead_id,
                Some(GroveBeadStatus::Failed),
                None,
                Some(Some(run.id.clone())),
                Some(None),
                Some(Some(FailureClass::Interrupted)),
                Some(Some(failure_detail.to_owned())),
                None,
                now,
            )?;

            insert_event_log_tx(
                &tx,
                EventKind::RecoveryActionTaken,
                Some(&run.bead_id),
                Some(&run.id),
                None,
                &serde_json::json!({
                    "action": "interrupt_active_run",
                    "previous_status": encode_run_status(RunStatus::Active),
                    "failure_class": encode_failure_class(FailureClass::Interrupted),
                }),
                now,
            )?;

            let recovery_capsule = RecoveryCapsule::from_parts(
                RecoveryCapsuleOutcome::Interrupted,
                Some(FailureClass::Interrupted),
                Some(failure_detail),
                None,
                None,
                None,
                None,
                &[],
            );
            if let Some(capsule) = recovery_capsule.as_ref() {
                insert_event_log_tx(
                    &tx,
                    EventKind::RecoveryCapsuleCreated,
                    Some(&run.bead_id),
                    Some(&run.id),
                    None,
                    &serde_json::to_value(capsule)
                        .context("serialize interrupted recovery capsule")?,
                    now,
                )?;
            }

            let raw = tx.query_row(
                "SELECT id, bead_id, attempt_no, status, failure_class, failure_detail, started_at, ended_at, session_count, checkpoint_count, last_checkpoint_id, activity, last_activity_at, escalation_tier \
                 FROM task_runs WHERE id = ?1",
                [run.id.as_str()],
                raw_task_run_row,
            )
            .with_context(|| format!("query interrupted run {}", run.id.as_str()))?;
            recovered.push(InterruptedRunRecovery {
                run: raw_task_run_into_record(raw)?,
                bead_id: run.bead_id.clone(),
                recovery_capsule,
            });
        }

        tx.commit()
            .context("commit interrupted run reconciliation transaction")?;
        Ok(recovered)
    }

    pub fn write_event_log(
        &mut self,
        kind: EventKind,
        bead_id: Option<&BeadId>,
        run_id: Option<&RunId>,
        session_id: Option<&SessionId>,
        payload: &serde_json::Value,
        created_at: &chrono::DateTime<Utc>,
    ) -> Result<()> {
        self.with_tx(|tx| {
            insert_event_log_tx(tx, kind, bead_id, run_id, session_id, payload, created_at)
        })
    }

    pub fn update_run_activity(
        &mut self,
        bead_id: &BeadId,
        run_id: &RunId,
        activity: grove_types::AgentActivity,
        updated_at: &chrono::DateTime<Utc>,
    ) -> Result<()> {
        self.with_tx(|tx| {
            ensure_bead_exists(tx, bead_id)?;
            ensure_run_exists(tx, run_id)?;
            ensure_run_belongs_to_bead(tx, run_id, bead_id)?;
            tx.execute(
                "UPDATE task_runs SET activity = ?2, last_activity_at = ?3 WHERE id = ?1",
                params![
                    run_id.as_str(),
                    encode_agent_activity(activity),
                    timestamp_string(updated_at),
                ],
            )
            .with_context(|| format!("update run activity {}", run_id.as_str()))?;
            insert_event_log_tx(
                tx,
                EventKind::ActivityStateChanged,
                Some(bead_id),
                Some(run_id),
                None,
                &serde_json::json!({
                    "activity": encode_agent_activity(activity),
                }),
                updated_at,
            )
        })
    }

    pub fn update_run_escalation_tier(
        &mut self,
        bead_id: &BeadId,
        run_id: &RunId,
        tier: grove_types::EscalationTier,
        updated_at: &chrono::DateTime<Utc>,
    ) -> Result<()> {
        self.with_tx(|tx| {
            ensure_bead_exists(tx, bead_id)?;
            ensure_run_exists(tx, run_id)?;
            ensure_run_belongs_to_bead(tx, run_id, bead_id)?;
            tx.execute(
                "UPDATE task_runs SET escalation_tier = ?2 WHERE id = ?1",
                params![run_id.as_str(), encode_escalation_tier(tier)],
            )
            .with_context(|| format!("update escalation tier {}", run_id.as_str()))?;
            insert_event_log_tx(
                tx,
                EventKind::EscalationTierChanged,
                Some(bead_id),
                Some(run_id),
                None,
                &serde_json::json!({
                    "escalation_tier": encode_escalation_tier(tier),
                }),
                updated_at,
            )
        })
    }

    pub fn list_event_logs_for_bead(&self, bead_id: &BeadId) -> Result<Vec<EventLogRecord>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, kind, bead_id, run_id, session_id, payload_json, created_at, \
                    correlation_id, operation, outcome, duration_ms, error_json, context_snapshot_json \
                 FROM event_log \
                 WHERE bead_id = ?1 \
                 ORDER BY id DESC",
            )
            .context("prepare event log list query")?;

        let rows = stmt
            .query_map([bead_id.as_str()], raw_event_log_row)
            .with_context(|| format!("query event log for {}", bead_id.as_str()))?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("collect event log rows")?
            .into_iter()
            .map(raw_event_log_into_record)
            .collect()
    }

    pub fn write_recovery_capsule(
        &mut self,
        input: RecoveryCapsuleWriteInput,
    ) -> Result<RecoveryCapsuleEvent> {
        let tx = self
            .conn
            .transaction()
            .context("begin recovery capsule write transaction")?;
        ensure_bead_exists(&tx, &input.bead_id)?;
        ensure_run_exists(&tx, &input.run_id)?;
        ensure_run_belongs_to_bead(&tx, &input.run_id, &input.bead_id)?;

        insert_event_log_tx(
            &tx,
            EventKind::RecoveryCapsuleCreated,
            Some(&input.bead_id),
            Some(&input.run_id),
            None,
            &serde_json::to_value(&input.capsule).context("serialize recovery capsule")?,
            &input.created_at,
        )?;

        let row = tx
            .query_row(
                "SELECT id, kind, bead_id, run_id, session_id, payload_json, created_at, \
                    correlation_id, operation, outcome, duration_ms, error_json, context_snapshot_json \
                 FROM event_log \
                 WHERE bead_id = ?1 AND run_id = ?2 AND kind = ?3 \
                 ORDER BY id DESC \
                 LIMIT 1",
                params![
                    input.bead_id.as_str(),
                    input.run_id.as_str(),
                    encode_event_kind(EventKind::RecoveryCapsuleCreated)
                ],
                raw_event_log_row,
            )
            .with_context(|| {
                format!(
                    "query inserted recovery capsule for {} run {}",
                    input.bead_id.as_str(),
                    input.run_id.as_str()
                )
            })?;
        tx.commit()
            .context("commit recovery capsule write transaction")?;
        raw_recovery_capsule_event_into_record(row)
    }

    pub fn latest_recovery_capsule_for_bead(
        &self,
        bead_id: &BeadId,
    ) -> Result<Option<RecoveryCapsuleEvent>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, kind, bead_id, run_id, session_id, payload_json, created_at, \
                    correlation_id, operation, outcome, duration_ms, error_json, context_snapshot_json \
                 FROM event_log \
                 WHERE bead_id = ?1 AND kind = ?2 \
                 ORDER BY id DESC \
                 LIMIT 1",
            )
            .context("prepare latest recovery capsule query")?;

        let row = stmt
            .query_row(
                params![
                    bead_id.as_str(),
                    encode_event_kind(EventKind::RecoveryCapsuleCreated)
                ],
                raw_event_log_row,
            )
            .optional()
            .with_context(|| format!("query latest recovery capsule for {}", bead_id.as_str()))?;

        row.map(raw_recovery_capsule_event_into_record).transpose()
    }

    pub fn list_active_reservations(&self) -> Result<Vec<ReservationRecord>> {
        self.list_active_reservations_at(&Utc::now())
    }

    pub fn list_active_reservations_at(
        &self,
        now: &chrono::DateTime<Utc>,
    ) -> Result<Vec<ReservationRecord>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, bead_id, run_id, path_pattern, exclusive, reason, expires_at, released_at \
                 FROM reservations \
                 WHERE released_at IS NULL \
                   AND expires_at > ?1 \
                 ORDER BY bead_id ASC, id ASC",
            )
            .context("prepare active reservation list query")?;

        let now = timestamp_string(now);
        let rows = stmt
            .query_map([&now], raw_reservation_row)
            .context("query active reservations")?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("collect active reservation rows")?
            .into_iter()
            .map(raw_reservation_into_record)
            .collect()
    }

    pub fn list_reservations_for_bead(&self, bead_id: &BeadId) -> Result<Vec<ReservationRecord>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, bead_id, run_id, path_pattern, exclusive, reason, expires_at, released_at \
                 FROM reservations \
                 WHERE bead_id = ?1 \
                 ORDER BY id ASC",
            )
            .context("prepare bead reservation list query")?;

        let rows = stmt
            .query_map([bead_id.as_str()], raw_reservation_row)
            .with_context(|| format!("query reservations for {}", bead_id.as_str()))?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("collect bead reservation rows")?
            .into_iter()
            .map(raw_reservation_into_record)
            .collect()
    }

    pub fn reset_bead_for_retry(
        &mut self,
        bead_id: &BeadId,
        now: &chrono::DateTime<Utc>,
    ) -> Result<()> {
        let tx = self
            .conn
            .transaction()
            .context("begin reset bead for retry transaction")?;

        tx.execute(
            "UPDATE bead_runtime \
             SET grove_status = ?2, retry_after = NULL, circuit_breaker_json = NULL, runtime_updated_at = ?3 \
             WHERE bead_id = ?1",
            params![
                bead_id.as_str(),
                encode_grove_bead_status(GroveBeadStatus::Ready),
                timestamp_string(now)
            ],
        )
        .with_context(|| format!("update bead_runtime for retry {}", bead_id.as_str()))?;

        insert_event_log_tx(
            &tx,
            EventKind::RecoveryActionTaken,
            Some(bead_id),
            None,
            None,
            &serde_json::json!({"action": "retry_reset"}),
            now,
        )?;

        tx.commit()
            .context("commit reset bead for retry transaction")?;
        Ok(())
    }

    pub fn latest_event_by_kind(&self, kind: EventKind) -> Result<Option<EventLogRecord>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, kind, bead_id, run_id, session_id, payload_json, created_at, \
                    correlation_id, operation, outcome, duration_ms, error_json, context_snapshot_json \
                 FROM event_log \
                 WHERE kind = ?1 \
                 ORDER BY id DESC LIMIT 1",
            )
            .context("prepare latest event by kind query")?;

        stmt.query_row([encode_event_kind(kind)], raw_event_log_row)
            .optional()
            .context("query latest event by kind")?
            .map(raw_event_log_into_record)
            .transpose()
    }

    pub fn list_events_for_run(&self, run_id: &RunId) -> Result<Vec<EventLogRecord>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, kind, bead_id, run_id, session_id, payload_json, created_at, \
                    correlation_id, operation, outcome, duration_ms, error_json, context_snapshot_json \
                 FROM event_log \
                 WHERE run_id = ?1 \
                 ORDER BY id ASC",
            )
            .context("prepare run event log list query")?;

        let rows = stmt
            .query_map([run_id.as_str()], raw_event_log_row)
            .with_context(|| format!("query event log for run {}", run_id.as_str()))?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("collect run event log rows")?
            .into_iter()
            .map(raw_event_log_into_record)
            .collect()
    }

    pub fn aggregate_run_metrics(&self, run_id: &RunId) -> Result<Option<grove_types::RunMetrics>> {
        let events = self.list_events_for_run(run_id)?;
        if events.is_empty() {
            return Ok(None);
        }

        let run = self
            .conn
            .query_row(
                "SELECT started_at, ended_at, checkpoint_count FROM task_runs WHERE id = ?1",
                [run_id.as_str()],
                |row| {
                    let started_at: String = row.get(0)?;
                    let ended_at: Option<String> = row.get(1)?;
                    let checkpoint_count: i32 = row.get(2)?;
                    Ok((started_at, ended_at, checkpoint_count))
                },
            )
            .optional()
            .context("query run for metrics aggregation")?;

        let Some((started_at, ended_at, checkpoint_count)) = run else {
            return Ok(None);
        };

        let started = parse_timestamp(&started_at)?;
        let ended = ended_at.as_ref().and_then(|s| parse_timestamp(s).ok());
        let total_duration_secs = ended
            .map(|e| (e - started).num_seconds() as u64)
            .unwrap_or(0);

        let checkpoints_taken = checkpoint_count as u32;
        let retries_attempted = events
            .iter()
            .filter(|e| e.kind == EventKind::RunStarted)
            .count()
            .saturating_sub(1) as u32;
        let rescue_injections = events
            .iter()
            .filter(|e| matches!(e.kind, EventKind::EscalationTierChanged))
            .count() as u32;
        let reactions_invoked = events
            .iter()
            .filter(|e| e.kind == EventKind::ReactionInvoked)
            .count() as u32;

        let max_escalation_tier = events
            .iter()
            .filter_map(|e| {
                e.payload
                    .get("escalation_tier")
                    .and_then(|v| v.as_str())
                    .and_then(|s| {
                        let normalized = s
                            .chars()
                            .filter(|c| c.is_alphanumeric())
                            .collect::<String>()
                            .to_lowercase();
                        match normalized.as_str() {
                            "firstattempt" => Some(0),
                            "secondattempt" => Some(1),
                            "thirdattempt" => Some(2),
                            "finalattempt" => Some(3),
                            "giveup" => Some(4),
                            _ => None,
                        }
                    })
            })
            .max()
            .unwrap_or(0);

        let termination_reason = events
            .iter()
            .find(|e| matches!(e.kind, EventKind::RunSucceeded | EventKind::RunFailed))
            .map(|e| format!("{:?}", e.kind));

        let termination_reason = termination_reason.or_else(|| {
            if ended.is_some() {
                Some("Ended".to_string())
            } else {
                None
            }
        });

        Ok(Some(grove_types::RunMetrics {
            run_id: run_id.clone(),
            total_duration_secs,
            checkpoints_taken,
            retries_attempted,
            rescue_injections,
            reactions_invoked,
            max_escalation_tier,
            termination_reason,
        }))
    }

    pub fn generate_run_report(&self, run_id: &RunId) -> Result<Option<grove_types::RunReport>> {
        use crate::RecoveryCapsuleEvent;
        use grove_types::RunReport;

        let events = self.list_events_for_run(run_id)?;
        if events.is_empty() {
            return Ok(None);
        }

        let runs = self
            .conn
            .query_row(
                "SELECT bead_id, status, failure_class FROM task_runs WHERE id = ?1",
                [run_id.as_str()],
                |row| {
                    let bead_id: String = row.get(0)?;
                    let status: String = row.get(1)?;
                    let failure_class: Option<String> = row.get(2)?;
                    Ok((bead_id, status, failure_class))
                },
            )
            .optional()
            .context("query run for report")?;

        let Some((bead_id_str, status_str, failure_class_str)) = runs else {
            return Ok(None);
        };

        let bead_id = BeadId::new(bead_id_str);
        let run_status = parse_run_status(&status_str)?;
        let failure_class = failure_class_str
            .as_ref()
            .and_then(|s| parse_failure_class(s).ok());

        let metrics =
            self.aggregate_run_metrics(run_id)?
                .unwrap_or_else(|| grove_types::RunMetrics {
                    run_id: run_id.clone(),
                    total_duration_secs: 0,
                    checkpoints_taken: 0,
                    retries_attempted: 0,
                    rescue_injections: 0,
                    reactions_invoked: 0,
                    max_escalation_tier: 0,
                    termination_reason: None,
                });

        let event_count = events.len() as u32;
        let first_event_at = events.first().map(|e| e.created_at);
        let last_event_at = events.last().map(|e| e.created_at);

        let recovery_capsule = self
            .latest_recovery_capsule_for_bead(&bead_id)?
            .map(|e: RecoveryCapsuleEvent| e.capsule);

        Ok(Some(RunReport {
            run_id: run_id.clone(),
            bead_id,
            status: run_status,
            metrics,
            failure_class,
            recovery_capsule,
            event_count,
            first_event_at,
            last_event_at,
        }))
    }

    pub fn acquire_reservations(
        &mut self,
        bead_id: &BeadId,
        run_id: Option<&RunId>,
        requests: &[ReservationRequest<'_>],
        acquired_at: &chrono::DateTime<Utc>,
    ) -> Result<ReservationAcquireOutcome> {
        let tx = self
            .conn
            .transaction()
            .context("begin reservation acquire transaction")?;
        ensure_bead_exists(&tx, bead_id)?;
        if let Some(run_id) = run_id {
            ensure_run_exists(&tx, run_id)?;
        }

        let active = list_active_reservations_tx(&tx, acquired_at)?;
        let mut conflicts = Vec::new();
        for request in requests {
            conflicts.extend(conflicts_for_request(bead_id, run_id, request, &active));
        }

        if !conflicts.is_empty() {
            for conflict in &conflicts {
                insert_event_log_tx(
                    &tx,
                    EventKind::ReservationConflictDetected,
                    Some(bead_id),
                    run_id,
                    None,
                    &serde_json::json!({
                        "requested_pattern": conflict.requested_pattern,
                        "held_pattern": conflict.held_pattern,
                        "conflicting_bead": conflict.conflicting_bead.as_str(),
                        "conflicting_run_id": conflict.conflicting_run_id.as_ref().map(RunId::as_str),
                    }),
                    acquired_at,
                )?;
            }
            tx.commit()
                .context("commit reservation conflict transaction")?;
            return Ok(ReservationAcquireOutcome {
                acquired: Vec::new(),
                conflicts,
            });
        }

        let mut acquired = Vec::with_capacity(requests.len());
        for request in requests {
            tx.execute(
                "INSERT INTO reservations(\
                    bead_id, run_id, path_pattern, exclusive, reason, expires_at, released_at\
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)",
                params![
                    bead_id.as_str(),
                    run_id.map(RunId::as_str),
                    request.path_pattern,
                    matches!(request.mode, ReservationMode::Exclusive),
                    request.reason,
                    timestamp_string(&request.expires_at),
                ],
            )
            .with_context(|| {
                format!(
                    "insert reservation {} for {}",
                    request.path_pattern,
                    bead_id.as_str()
                )
            })?;
            let reservation_id = tx.last_insert_rowid();
            let record = ReservationRecord {
                id: reservation_id,
                bead_id: bead_id.clone(),
                run_id: run_id.cloned(),
                path_pattern: request.path_pattern.to_owned(),
                mode: request.mode,
                reason: request.reason.map(ToOwned::to_owned),
                expires_at: request.expires_at,
                released_at: None,
            };
            insert_event_log_tx(
                &tx,
                EventKind::ReservationGranted,
                Some(bead_id),
                run_id,
                None,
                &serde_json::json!({
                    "reservation_id": reservation_id,
                    "path_pattern": record.path_pattern,
                    "mode": encode_reservation_mode(record.mode),
                    "reason": record.reason,
                    "expires_at": record.expires_at.to_rfc3339(),
                }),
                acquired_at,
            )?;
            acquired.push(record);
        }

        if !acquired.is_empty() {
            set_declared_paths_tx(
                &tx,
                bead_id,
                run_id,
                &active_declared_paths_tx(&tx, bead_id, acquired_at)?,
            )?;
        }

        tx.commit()
            .context("commit reservation acquire transaction")?;
        Ok(ReservationAcquireOutcome {
            acquired,
            conflicts: Vec::new(),
        })
    }

    pub fn release_reservations_for_run(
        &mut self,
        bead_id: &BeadId,
        run_id: &RunId,
        released_at: &chrono::DateTime<Utc>,
    ) -> Result<Vec<ReservationRecord>> {
        self.release_reservations_matching(bead_id, Some(run_id), None, released_at)
    }

    pub fn release_reservations_for_bead(
        &mut self,
        bead_id: &BeadId,
        released_at: &chrono::DateTime<Utc>,
    ) -> Result<Vec<ReservationRecord>> {
        self.release_reservations_matching(bead_id, None, None, released_at)
    }

    pub fn expire_reservations(
        &mut self,
        now: &chrono::DateTime<Utc>,
    ) -> Result<Vec<ReservationRecord>> {
        let tx = self
            .conn
            .transaction()
            .context("begin reservation expiry transaction")?;
        let expired = list_expired_unreleased_reservations_tx(&tx, now)?;
        for record in &expired {
            mark_reservation_released_tx(&tx, record.id, now)?;
            insert_event_log_tx(
                &tx,
                EventKind::ReservationExpired,
                Some(&record.bead_id),
                record.run_id.as_ref(),
                None,
                &serde_json::json!({
                    "reservation_id": record.id,
                    "path_pattern": record.path_pattern,
                    "expired_at": timestamp_string(now),
                }),
                now,
            )?;
        }
        refresh_declared_paths_for_beads_tx(
            &tx,
            expired.iter().map(|r| r.bead_id.clone()).collect(),
            now,
        )?;
        tx.commit()
            .context("commit reservation expiry transaction")?;
        Ok(expired)
    }

    pub fn recover_stale_reservations(
        &mut self,
        now: &chrono::DateTime<Utc>,
    ) -> Result<Vec<RecoveredReservation>> {
        let tx = self
            .conn
            .transaction()
            .context("begin reservation recovery transaction")?;
        let active = list_active_reservations_tx(&tx, now)?;
        let stale = active
            .into_iter()
            .filter(|record| {
                record
                    .run_id
                    .as_ref()
                    .is_some_and(|run_id| is_run_terminal_tx(&tx, run_id).unwrap_or(false))
            })
            .collect::<Vec<_>>();

        let mut recovered = Vec::with_capacity(stale.len());
        for record in stale {
            mark_reservation_released_tx(&tx, record.id, now)?;
            let run_terminal = record
                .run_id
                .as_ref()
                .map(|run_id| run_status_for_event_tx(&tx, run_id))
                .transpose()?;
            insert_event_log_tx(
                &tx,
                EventKind::RecoveryActionTaken,
                Some(&record.bead_id),
                record.run_id.as_ref(),
                None,
                &serde_json::json!({
                    "action": "release_stale_reservation",
                    "reservation_id": record.id,
                    "path_pattern": record.path_pattern,
                    "run_status": run_terminal,
                }),
                now,
            )?;
            recovered.push(RecoveredReservation {
                reservation: record,
                reason: RecoveryReason::RunNoLongerActive,
            });
        }

        refresh_declared_paths_for_beads_tx(
            &tx,
            recovered
                .iter()
                .map(|entry| entry.reservation.bead_id.clone())
                .collect(),
            now,
        )?;
        tx.commit()
            .context("commit reservation recovery transaction")?;
        Ok(recovered)
    }

    fn release_reservations_matching(
        &mut self,
        bead_id: &BeadId,
        run_id: Option<&RunId>,
        path_patterns: Option<&[String]>,
        released_at: &chrono::DateTime<Utc>,
    ) -> Result<Vec<ReservationRecord>> {
        let tx = self
            .conn
            .transaction()
            .context("begin reservation release transaction")?;
        let matching = list_releasable_reservations_tx(&tx, bead_id, run_id, path_patterns)?;
        for record in &matching {
            mark_reservation_released_tx(&tx, record.id, released_at)?;
            insert_event_log_tx(
                &tx,
                EventKind::RecoveryActionTaken,
                Some(&record.bead_id),
                record.run_id.as_ref(),
                None,
                &serde_json::json!({
                    "action": "release_reservation",
                    "reservation_id": record.id,
                    "path_pattern": record.path_pattern,
                    "released_at": timestamp_string(released_at),
                }),
                released_at,
            )?;
        }
        refresh_declared_paths_for_beads_tx(&tx, vec![bead_id.clone()], released_at)?;
        tx.commit()
            .context("commit reservation release transaction")?;
        Ok(matching)
    }

    // Mirror outbox methods for durable br sync retries (grove-1j9.7.6)

    pub fn enqueue_mirror_outbox(
        &mut self,
        bead_id: &BeadId,
        run_id: &RunId,
        handoff: &HandoffRecord,
        close_bead: bool,
    ) -> Result<MirrorOutboxRecord> {
        let tx = self
            .conn
            .transaction()
            .context("begin enqueue mirror outbox transaction")?;
        let id = format!("mirror-{}-{}", bead_id.as_str(), run_id.as_str());
        let now = now_timestamp_string();
        let handoff_json =
            serde_json::to_string(handoff).context("serialize handoff for mirror outbox")?;

        tx.execute(
            "INSERT INTO mirror_outbox(id, bead_id, run_id, handoff_json, close_bead, mirror_status, attempt_count, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                id,
                bead_id.as_str(),
                run_id.as_str(),
                handoff_json,
                close_bead as i32,
                "pending",
                0i32,
                now,
                now,
            ],
        ).context("insert mirror outbox record")?;

        insert_event_log_tx(
            &tx,
            EventKind::BrMirrorRequested,
            Some(bead_id),
            Some(run_id),
            None,
            &serde_json::json!({
                "mirror_outbox_id": id,
                "close_bead": close_bead,
                "handoff_summary": handoff.summary,
            }),
            &Utc::now(),
        )?;

        tx.commit()
            .context("commit enqueue mirror outbox transaction")?;

        Ok(MirrorOutboxRecord {
            id,
            bead_id: bead_id.clone(),
            run_id: run_id.clone(),
            handoff: handoff.clone(),
            close_bead,
            mirror_status: MirrorStatus::Pending,
            attempt_count: 0,
            last_attempt_at: None,
            next_retry_after: None,
            last_error: None,
            created_at: now.parse().context("parse created timestamp")?,
            updated_at: now.parse().context("parse updated timestamp")?,
        })
    }

    pub fn list_pending_mirror_operations(&self, limit: i32) -> Result<Vec<MirrorOutboxRecord>> {
        let now = now_timestamp_string();

        self.conn
            .prepare(
                "SELECT id, bead_id, run_id, handoff_json, close_bead, mirror_status, \
                 attempt_count, last_attempt_at, next_retry_after, last_error, created_at, updated_at \
                 FROM mirror_outbox \
                 WHERE mirror_status IN ('pending', 'failed') \
                 AND (next_retry_after IS NULL OR next_retry_after <= ?1) \
                 ORDER BY created_at ASC \
                 LIMIT ?2"
            )
            .context("prepare list pending mirror operations query")?
            .query_map(params![now, limit], |row| {
                let handoff_json: String = row.get(3)?;
                let handoff: HandoffRecord = serde_json::from_str(&handoff_json)
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e) as Box<dyn std::error::Error + Send + Sync>))?;

                Ok(MirrorOutboxRecord {
                    id: row.get(0)?,
                    bead_id: BeadId::new(row.get::<_, String>(1)?),
                    run_id: RunId::new(row.get::<_, String>(2)?),
                    handoff,
                    close_bead: row.get::<_, i32>(4)? != 0,
                    mirror_status: mirror_status_from_str(row.get::<_, String>(5)?.as_str())
                        .ok_or_else(|| rusqlite::Error::InvalidQuery)?,
                    attempt_count: row.get(6)?,
                    last_attempt_at: row.get::<_, Option<String>>(7)?
                        .map(|s| s.parse().ok())
                        .flatten(),
                    next_retry_after: row.get::<_, Option<String>>(8)?
                        .map(|s| s.parse().ok())
                        .flatten(),
                    last_error: row.get(9)?,
                    created_at: row.get::<_, String>(10)?.parse().ok()
                        .ok_or_else(|| rusqlite::Error::InvalidQuery)?,
                    updated_at: row.get::<_, String>(11)?.parse().ok()
                        .ok_or_else(|| rusqlite::Error::InvalidQuery)?,
                })
            })
            .context("execute list pending mirror operations query")?
            .collect::<Result<Vec<_>, _>>()
            .context("collect pending mirror operations")
    }

    pub fn mark_mirror_in_progress(&mut self, id: &str) -> Result<()> {
        let now = now_timestamp_string();
        self.conn
            .execute(
                "UPDATE mirror_outbox \
                 SET mirror_status = 'in_progress', \
                     attempt_count = attempt_count + 1, \
                     last_attempt_at = ?1, \
                     updated_at = ?1 \
                 WHERE id = ?2",
                params![now, id],
            )
            .context("mark mirror operation as in progress")?;
        Ok(())
    }

    pub fn record_mirror_success(&mut self, id: &str, run_id: &RunId) -> Result<()> {
        let tx = self
            .conn
            .transaction()
            .context("begin record mirror success transaction")?;
        let now = now_timestamp_string();
        let bead_id: Option<String> = tx
            .query_row(
                "SELECT bead_id FROM mirror_outbox WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .optional()
            .context("lookup bead id for mirror success")?;

        tx.execute(
            "UPDATE mirror_outbox \
             SET mirror_status = 'succeeded', \
                 last_attempt_at = COALESCE(last_attempt_at, ?1), \
                 next_retry_after = NULL, \
                 last_error = NULL, \
                 updated_at = ?1 \
             WHERE id = ?2",
            params![now, id],
        )
        .context("update mirror outbox status to succeeded")?;

        insert_event_log_tx(
            &tx,
            EventKind::BrMirrorSucceeded,
            bead_id.as_ref().map(|id| BeadId::new(id.clone())).as_ref(),
            Some(run_id),
            None,
            &serde_json::json!({
                "mirror_outbox_id": id,
            }),
            &Utc::now(),
        )?;

        tx.commit()
            .context("commit record mirror success transaction")?;
        Ok(())
    }

    pub fn record_mirror_failure(
        &mut self,
        id: &str,
        run_id: &RunId,
        error: &str,
        retry_after: Option<&chrono::DateTime<Utc>>,
    ) -> Result<()> {
        let tx = self
            .conn
            .transaction()
            .context("begin record mirror failure transaction")?;
        let now = now_timestamp_string();
        let retry_after_str = retry_after.map(|dt| timestamp_string(dt));
        let bead_id: Option<String> = tx
            .query_row(
                "SELECT bead_id FROM mirror_outbox WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .optional()
            .context("lookup bead id for mirror failure")?;

        tx.execute(
            "UPDATE mirror_outbox \
             SET mirror_status = 'failed', \
                 next_retry_after = ?1, \
                 last_error = ?2, \
                 last_attempt_at = COALESCE(last_attempt_at, ?3), \
                 updated_at = ?3 \
             WHERE id = ?4",
            params![retry_after_str, error, now, id],
        )
        .context("update mirror outbox with failure details")?;

        insert_event_log_tx(
            &tx,
            EventKind::BrMirrorFailed,
            bead_id.as_ref().map(|id| BeadId::new(id.clone())).as_ref(),
            Some(run_id),
            None,
            &serde_json::json!({
                "mirror_outbox_id": id,
                "error": error,
            }),
            &Utc::now(),
        )?;

        tx.commit()
            .context("commit record mirror failure transaction")?;
        Ok(())
    }

    pub fn mirror_status_for_bead(&self, bead_id: &BeadId) -> Result<Option<MirrorStatus>> {
        self.conn
            .query_row(
                "SELECT mirror_status FROM mirror_outbox WHERE bead_id = ?1 ORDER BY created_at DESC LIMIT 1",
                [bead_id.as_str()],
                |row| {
                    let status_str: String = row.get(0)?;
                    mirror_status_from_str(&status_str)
                        .ok_or_else(|| rusqlite::Error::InvalidQuery)
                },
            )
            .optional()
            .context("query mirror status for bead")
    }

    pub fn pending_mirror_count_for_bead(&self, bead_id: &BeadId) -> Result<i64> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM mirror_outbox WHERE bead_id = ?1 AND mirror_status IN ('pending', 'failed')",
                [bead_id.as_str()],
                |row| row.get(0),
            )
            .context("count pending mirror operations for bead")
    }

    pub fn list_unresolved_mirror_operations_for_bead(
        &self,
        bead_id: &BeadId,
    ) -> Result<Vec<MirrorOutboxRecord>> {
        self.conn
            .prepare(
                "SELECT id, bead_id, run_id, handoff_json, close_bead, mirror_status, \
                 attempt_count, last_attempt_at, next_retry_after, last_error, created_at, updated_at \
                 FROM mirror_outbox \
                 WHERE bead_id = ?1 AND mirror_status != 'succeeded' \
                 ORDER BY created_at DESC"
            )
            .context("prepare list unresolved mirror operations for bead query")?
            .query_map([bead_id.as_str()], |row| {
                let handoff_json: String = row.get(3)?;
                let handoff: HandoffRecord = serde_json::from_str(&handoff_json)
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e) as Box<dyn std::error::Error + Send + Sync>))?;

                Ok(MirrorOutboxRecord {
                    id: row.get(0)?,
                    bead_id: BeadId::new(row.get::<_, String>(1)?),
                    run_id: RunId::new(row.get::<_, String>(2)?),
                    handoff,
                    close_bead: row.get::<_, i32>(4)? != 0,
                    mirror_status: mirror_status_from_str(row.get::<_, String>(5)?.as_str())
                        .ok_or_else(|| rusqlite::Error::InvalidQuery)?,
                    attempt_count: row.get(6)?,
                    last_attempt_at: row.get::<_, Option<String>>(7)?
                        .map(|s| s.parse().ok())
                        .flatten(),
                    next_retry_after: row.get::<_, Option<String>>(8)?
                        .map(|s| s.parse().ok())
                        .flatten(),
                    last_error: row.get(9)?,
                    created_at: row.get::<_, String>(10)?.parse().ok()
                        .ok_or_else(|| rusqlite::Error::InvalidQuery)?,
                    updated_at: row.get::<_, String>(11)?.parse().ok()
                        .ok_or_else(|| rusqlite::Error::InvalidQuery)?,
                })
            })
            .context("execute list unresolved mirror operations for bead query")?
            .collect::<Result<Vec<_>, _>>()
            .context("collect unresolved mirror operations for bead")
    }

    fn applied_migration_name(&self, version: i64) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT name FROM _migrations WHERE version = ?1",
                [version],
                |row| row.get(0),
            )
            .optional()
            .with_context(|| format!("lookup applied migration version {version}"))
    }

    fn apply_migration(&mut self, migration: Migration<'_>) -> Result<()> {
        let tx = self
            .conn
            .transaction()
            .with_context(|| format!("begin migration {} transaction", migration.name))?;
        tx.execute_batch(migration.sql)
            .with_context(|| format!("execute migration {}", migration.name))?;
        tx.execute(
            "INSERT INTO _migrations(version, name) VALUES (?1, ?2)",
            (migration.version, migration.name),
        )
        .with_context(|| format!("record migration {}", migration.name))?;
        tx.commit()
            .with_context(|| format!("commit migration {}", migration.name))?;
        Ok(())
    }
}

impl BeadCacheStore for Database {
    fn upsert_bead_cache(&mut self, bead: &BrIssueSummary) -> Result<UpsertOutcome> {
        let existed = self
            .conn
            .query_row(
                "SELECT 1 FROM bead_cache WHERE bead_id = ?1",
                [bead.id.as_str()],
                |_| Ok(()),
            )
            .optional()
            .context("check existing bead cache row")?
            .is_some();

        let labels_json = serde_json::to_string(&bead.labels).context("serialize bead labels")?;
        let dependency_ids_json =
            serde_json::to_string(&bead.blocked_by).context("serialize blocked_by ids")?;
        let dependent_ids_json =
            serde_json::to_string(&bead.blocks).context("serialize dependent ids")?;
        let raw_json = serde_json::to_string(&bead.raw_json).context("serialize raw bead JSON")?;
        let synced_at = now_timestamp_string();

        self.conn
            .execute(
                "INSERT INTO bead_cache(\
                    bead_id, title, description, priority, issue_type, status, assignee,\
                    labels_json, parent_ids_json, dependency_ids_json, dependent_ids_json, raw_json, synced_at\
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, '[]', ?9, ?10, ?11, ?12) \
                ON CONFLICT(bead_id) DO UPDATE SET \
                    title = excluded.title, \
                    description = excluded.description, \
                    priority = excluded.priority, \
                    issue_type = excluded.issue_type, \
                    status = excluded.status, \
                    assignee = excluded.assignee, \
                    labels_json = excluded.labels_json, \
                    parent_ids_json = excluded.parent_ids_json, \
                    dependency_ids_json = excluded.dependency_ids_json, \
                    dependent_ids_json = excluded.dependent_ids_json, \
                    raw_json = excluded.raw_json, \
                    synced_at = excluded.synced_at",
                params![
                    bead.id.as_str(),
                    &bead.title,
                    bead.description.as_deref(),
                    bead_priority_to_db(bead.priority),
                    &bead.issue_type,
                    &bead.status,
                    bead.assignee.as_deref(),
                    &labels_json,
                    &dependency_ids_json,
                    &dependent_ids_json,
                    &raw_json,
                    &synced_at,
                ],
            )
            .with_context(|| format!("upsert bead cache row for {}", bead.id.as_str()))?;

        Ok(if existed {
            UpsertOutcome::Updated
        } else {
            UpsertOutcome::Added
        })
    }

    fn replace_dependency_snapshot(
        &mut self,
        bead_id: &BeadId,
        blocked_by: &[BeadId],
        blocks: &[BeadId],
    ) -> Result<()> {
        let tx = self.conn.transaction().with_context(|| {
            format!("begin dependency snapshot update for {}", bead_id.as_str())
        })?;

        tx.execute(
            "DELETE FROM bead_dependencies \
             WHERE relation_type = 'blocks' \
               AND (parent_id = ?1 OR child_id = ?1)",
            [bead_id.as_str()],
        )
        .with_context(|| format!("clear dependency snapshot for {}", bead_id.as_str()))?;

        let synced_at = now_timestamp_string();
        for dependency_id in blocked_by {
            tx.execute(
                "INSERT INTO bead_dependencies(parent_id, child_id, relation_type, synced_at) \
                 VALUES (?1, ?2, 'blocks', ?3)",
                params![dependency_id.as_str(), bead_id.as_str(), &synced_at],
            )
            .with_context(|| {
                format!(
                    "insert blocking dependency edge {} -> {}",
                    dependency_id.as_str(),
                    bead_id.as_str()
                )
            })?;
        }

        for dependent_id in blocks {
            tx.execute(
                "INSERT INTO bead_dependencies(parent_id, child_id, relation_type, synced_at) \
                 VALUES (?1, ?2, 'blocks', ?3)",
                params![bead_id.as_str(), dependent_id.as_str(), &synced_at],
            )
            .with_context(|| {
                format!(
                    "insert dependent edge {} -> {}",
                    bead_id.as_str(),
                    dependent_id.as_str()
                )
            })?;
        }

        tx.commit()
            .with_context(|| format!("commit dependency snapshot for {}", bead_id.as_str()))?;
        Ok(())
    }

    fn list_cached_beads(&self) -> Result<Vec<CachedBeadState>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT c.bead_id, r.grove_status \
                 FROM bead_cache c \
                 LEFT JOIN bead_runtime r ON r.bead_id = c.bead_id \
                 ORDER BY c.bead_id ASC",
            )
            .context("prepare cached bead list query")?;

        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })
            .context("query cached bead states")?;

        let entries = rows
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect cached bead states")?;

        entries
            .into_iter()
            .map(|(bead_id, grove_status)| {
                Ok(CachedBeadState {
                    bead_id: BeadId::new(bead_id),
                    grove_status: grove_status
                        .as_deref()
                        .map(parse_grove_bead_status)
                        .transpose()?,
                })
            })
            .collect()
    }

    fn set_grove_status(&mut self, bead_id: &BeadId, status: GroveBeadStatus) -> Result<()> {
        let runtime_updated_at = now_timestamp_string();
        self.conn
            .execute(
                "INSERT INTO bead_runtime(\
                    bead_id, grove_status, declared_paths_json, metadata_json, last_run_id, retry_after,\
                    last_failure_class, last_failure_detail, circuit_breaker_json, runtime_updated_at\
                 ) VALUES (?1, ?2, '[]', '{}', NULL, NULL, NULL, NULL, NULL, ?3) \
                 ON CONFLICT(bead_id) DO UPDATE SET \
                    grove_status = excluded.grove_status, \
                    runtime_updated_at = excluded.runtime_updated_at",
                params![
                    bead_id.as_str(),
                    encode_grove_bead_status(status),
                    &runtime_updated_at,
                ],
            )
            .with_context(|| format!("set grove status for {}", bead_id.as_str()))?;
        Ok(())
    }
}

pub fn migrations_dir() -> &'static str {
    "migrations"
}

fn apply_pragmas(conn: &Connection) -> Result<()> {
    for pragma in PRAGMAS {
        conn.execute_batch(pragma)
            .with_context(|| format!("apply SQLite pragma {pragma}"))?;
    }
    Ok(())
}

fn ensure_migration_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS _migrations (\
            version INTEGER PRIMARY KEY,\
            name TEXT NOT NULL,\
            applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP\
        );",
    )
    .context("ensure _migrations table exists")?;
    Ok(())
}

fn utf8_to_std_path(path: &Utf8Path) -> Result<PathBuf> {
    let std_path = Utf8PathBuf::from(path).into_std_path_buf();
    if std_path.as_os_str().is_empty() {
        bail!("database path resolved to an empty path from {path}");
    }
    Ok(std_path)
}

fn raw_bead_record_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawBeadRecordRow> {
    Ok(RawBeadRecordRow {
        bead_id: row.get(0)?,
        title: row.get(1)?,
        description: row.get(2)?,
        priority: row.get(3)?,
        issue_type: row.get(4)?,
        br_status: row.get(5)?,
        assignee: row.get(6)?,
        labels_json: row.get(7)?,
        raw_json: row.get(8)?,
        synced_at: row.get(9)?,
        grove_status: row.get(10)?,
        declared_paths_json: row.get(11)?,
        metadata_json: row.get(12)?,
        last_run_id: row.get(13)?,
        retry_after: row.get(14)?,
        last_failure_class: row.get(15)?,
        last_failure_detail: row.get(16)?,
        circuit_breaker_json: row.get(17)?,
        runtime_updated_at: row.get(18)?,
    })
}

fn raw_task_run_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawTaskRunRow> {
    Ok(RawTaskRunRow {
        id: row.get(0)?,
        bead_id: row.get(1)?,
        attempt_no: row.get(2)?,
        status: row.get(3)?,
        failure_class: row.get(4)?,
        failure_detail: row.get(5)?,
        started_at: row.get(6)?,
        ended_at: row.get(7)?,
        session_count: row.get(8)?,
        checkpoint_count: row.get(9)?,
        last_checkpoint_id: row.get(10)?,
        activity: row.get(11)?,
        last_activity_at: row.get(12)?,
        escalation_tier: row.get(13)?,
    })
}

fn raw_session_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawSessionRow> {
    Ok(RawSessionRow {
        id: row.get(0)?,
        run_id: row.get(1)?,
        external_session_id: row.get(2)?,
        ordinal_in_run: row.get(3)?,
        status: row.get(4)?,
        started_at: row.get(5)?,
        ended_at: row.get(6)?,
        prompt_id: row.get(7)?,
        prompt_manifest_path: row.get(8)?,
        prompt_bytes: row.get(9)?,
        estimated_input_tokens: row.get(10)?,
        estimated_output_tokens: row.get(11)?,
        exit_code: row.get(12)?,
        stop_reason: row.get(13)?,
        transcript_path: row.get(14)?,
    })
}

fn raw_checkpoint_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawCheckpointRow> {
    Ok(RawCheckpointRow {
        id: row.get(0)?,
        bead_id: row.get(1)?,
        run_id: row.get(2)?,
        session_id: row.get(3)?,
        progress: row.get(4)?,
        next_step: row.get(5)?,
        payload_json: row.get(6)?,
        saved_at: row.get(7)?,
        resume_generation: row.get(8)?,
    })
}

fn raw_handoff_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawHandoffRow> {
    Ok(RawHandoffRow {
        bead_id: row.get(0)?,
        run_id: row.get(1)?,
        summary: row.get(2)?,
        artifacts_json: row.get(3)?,
        lessons_json: row.get(4)?,
        decisions_json: row.get(5)?,
        warnings_json: row.get(6)?,
        completed_at: row.get(7)?,
    })
}

fn raw_event_log_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawEventLogRow> {
    Ok(RawEventLogRow {
        id: row.get(0)?,
        kind: row.get(1)?,
        bead_id: row.get(2)?,
        run_id: row.get(3)?,
        session_id: row.get(4)?,
        payload_json: row.get(5)?,
        created_at: row.get(6)?,
        correlation_id: row.get(7)?,
        operation: row.get(8)?,
        outcome: row.get(9)?,
        duration_ms: row.get(10)?,
        error_json: row.get(11)?,
        context_snapshot_json: row.get(12)?,
    })
}

fn raw_reservation_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawReservationRow> {
    Ok(RawReservationRow {
        id: row.get(0)?,
        bead_id: row.get(1)?,
        run_id: row.get(2)?,
        path_pattern: row.get(3)?,
        exclusive: row.get::<_, i64>(4)? != 0,
        reason: row.get(5)?,
        expires_at: row.get(6)?,
        released_at: row.get(7)?,
    })
}

fn raw_leader_lease_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawLeaderLeaseRow> {
    Ok(RawLeaderLeaseRow {
        owner_label: row.get(0)?,
        run_id: row.get(1)?,
        acquired_at: row.get(2)?,
        heartbeat_at: row.get(3)?,
        expires_at: row.get(4)?,
        released_at: row.get(5)?,
    })
}

fn raw_bead_record_into_record(row: RawBeadRecordRow) -> Result<GroveBeadRecord> {
    let raw_json: Value = parse_json(&row.raw_json, "raw bead JSON")?;
    let synced_at = parse_timestamp(&row.synced_at)?;
    let created_at = raw_issue_timestamp(&raw_json, "created_at")?.unwrap_or(synced_at);
    let updated_at = raw_issue_timestamp(&raw_json, "updated_at")?.unwrap_or(created_at);
    let runtime_updated_at = row
        .runtime_updated_at
        .as_deref()
        .map(parse_timestamp)
        .transpose()?
        .unwrap_or(updated_at);

    Ok(GroveBeadRecord {
        bead: BeadRef {
            id: BeadId::new(row.bead_id),
            title: row.title,
            description: row.description,
            priority: parse_bead_priority(row.priority)?,
            issue_type: row.issue_type,
            br_status: row.br_status,
            assignee: row.assignee,
            labels: parse_json(&row.labels_json, "bead labels")?,
            created_at,
            updated_at,
        },
        grove_status: row
            .grove_status
            .as_deref()
            .map(parse_grove_bead_status)
            .transpose()?
            .unwrap_or(GroveBeadStatus::Idle),
        declared_paths: parse_json(
            row.declared_paths_json.as_deref().unwrap_or("[]"),
            "declared paths",
        )?,
        metadata: parse_json(
            row.metadata_json.as_deref().unwrap_or("{}"),
            "runtime metadata",
        )?,
        last_run_id: row.last_run_id.map(RunId::new),
        retry_after: row
            .retry_after
            .as_deref()
            .map(parse_timestamp)
            .transpose()?,
        last_failure_class: row
            .last_failure_class
            .as_deref()
            .map(parse_failure_class)
            .transpose()?,
        last_failure_detail: row.last_failure_detail,
        circuit_breaker_state: row
            .circuit_breaker_json
            .as_deref()
            .map(|text| parse_json(text, "circuit breaker state"))
            .transpose()?,
        synced_at,
        runtime_updated_at,
    })
}

fn raw_task_run_into_record(row: RawTaskRunRow) -> Result<TaskRunRecord> {
    Ok(TaskRunRecord {
        id: RunId::new(row.id),
        bead_id: BeadId::new(row.bead_id),
        attempt_no: row.attempt_no,
        status: parse_run_status(&row.status)?,
        failure_class: row
            .failure_class
            .as_deref()
            .map(parse_failure_class)
            .transpose()?,
        failure_detail: row.failure_detail,
        started_at: parse_timestamp(&row.started_at)?,
        ended_at: row.ended_at.as_deref().map(parse_timestamp).transpose()?,
        session_count: row.session_count,
        checkpoint_count: row.checkpoint_count,
        last_checkpoint_id: row.last_checkpoint_id.map(CheckpointId::new),
        activity: row
            .activity
            .as_deref()
            .map(parse_agent_activity)
            .transpose()?,
        last_activity_at: row
            .last_activity_at
            .as_deref()
            .map(parse_timestamp)
            .transpose()?,
        escalation_tier: parse_escalation_tier(&row.escalation_tier)?,
    })
}

fn raw_session_into_record(row: RawSessionRow) -> Result<ClaudeSessionRecord> {
    Ok(ClaudeSessionRecord {
        id: SessionId::new(row.id),
        run_id: RunId::new(row.run_id),
        external_session_id: row.external_session_id,
        ordinal_in_run: row.ordinal_in_run,
        status: parse_session_status(&row.status)?,
        started_at: parse_timestamp(&row.started_at)?,
        ended_at: row.ended_at.as_deref().map(parse_timestamp).transpose()?,
        prompt_id: row.prompt_id.map(PromptId::new),
        prompt_manifest_path: row.prompt_manifest_path,
        prompt_bytes: row.prompt_bytes,
        estimated_input_tokens: row.estimated_input_tokens,
        estimated_output_tokens: row.estimated_output_tokens,
        exit_code: row.exit_code,
        stop_reason: row
            .stop_reason
            .as_deref()
            .map(parse_stop_reason)
            .transpose()?,
        transcript_path: row.transcript_path,
    })
}

fn raw_checkpoint_into_record(row: RawCheckpointRow) -> Result<CheckpointRecord> {
    Ok(CheckpointRecord {
        id: CheckpointId::new(row.id),
        bead_id: BeadId::new(row.bead_id),
        run_id: RunId::new(row.run_id),
        session_id: SessionId::new(row.session_id),
        progress: row.progress,
        next_step: row.next_step,
        payload: parse_json(&row.payload_json, "checkpoint payload")?,
        saved_at: parse_timestamp(&row.saved_at)?,
        resume_generation: row.resume_generation,
    })
}

fn raw_handoff_into_record(row: RawHandoffRow) -> Result<HandoffRecord> {
    Ok(HandoffRecord {
        bead_id: BeadId::new(row.bead_id),
        run_id: RunId::new(row.run_id),
        summary: row.summary,
        artifacts: parse_json(&row.artifacts_json, "handoff artifacts")?,
        lessons: parse_json(&row.lessons_json, "handoff lessons")?,
        decisions: parse_json(&row.decisions_json, "handoff decisions")?,
        warnings: parse_json(&row.warnings_json, "handoff warnings")?,
        completed_at: parse_timestamp(&row.completed_at)?,
    })
}

fn raw_event_log_into_record(row: RawEventLogRow) -> Result<EventLogRecord> {
    let error = row
        .error_json
        .as_ref()
        .and_then(|json| parse_json::<EventError>(json, "event error").ok());
    let context_snapshot = row
        .context_snapshot_json
        .as_ref()
        .and_then(|json| parse_json::<ContextSnapshot>(json, "context snapshot").ok());
    let outcome = row.outcome.as_ref().and_then(|s| parse_event_outcome(s));

    Ok(EventLogRecord {
        id: row.id,
        kind: parse_event_kind(&row.kind)?,
        bead_id: row.bead_id.map(BeadId::new),
        run_id: row.run_id.map(RunId::new),
        session_id: row.session_id.map(SessionId::new),
        payload: parse_json(&row.payload_json, "event log payload")?,
        created_at: parse_timestamp(&row.created_at)?,
        correlation_id: row.correlation_id,
        operation: row.operation,
        outcome,
        duration_ms: row.duration_ms.map(|ms| ms as u64),
        error,
        context_snapshot,
    })
}

fn raw_recovery_capsule_event_into_record(row: RawEventLogRow) -> Result<RecoveryCapsuleEvent> {
    let created_at = parse_timestamp(&row.created_at)?;
    Ok(RecoveryCapsuleEvent {
        capsule: parse_json(&row.payload_json, "recovery capsule event payload")?,
        source_event_id: row.id,
        created_at,
    })
}

fn raw_leader_lease_into_record(row: RawLeaderLeaseRow) -> Result<LeaderLeaseRecord> {
    Ok(LeaderLeaseRecord {
        owner_label: row.owner_label,
        run_id: row.run_id.map(RunId::new),
        acquired_at: parse_timestamp(&row.acquired_at)?,
        heartbeat_at: parse_timestamp(&row.heartbeat_at)?,
        expires_at: parse_timestamp(&row.expires_at)?,
        released_at: row
            .released_at
            .as_deref()
            .map(parse_timestamp)
            .transpose()?,
    })
}

fn raw_reservation_into_record(row: RawReservationRow) -> Result<ReservationRecord> {
    Ok(ReservationRecord {
        id: row.id,
        bead_id: BeadId::new(row.bead_id),
        run_id: row.run_id.map(RunId::new),
        path_pattern: row.path_pattern,
        mode: if row.exclusive {
            ReservationMode::Exclusive
        } else {
            ReservationMode::Shared
        },
        reason: row.reason,
        expires_at: parse_timestamp(&row.expires_at)?,
        released_at: row
            .released_at
            .as_deref()
            .map(parse_timestamp)
            .transpose()?,
    })
}

fn ensure_bead_exists(tx: &Transaction<'_>, bead_id: &BeadId) -> Result<()> {
    let exists = tx
        .query_row(
            "SELECT 1 FROM bead_cache WHERE bead_id = ?1",
            [bead_id.as_str()],
            |_| Ok(()),
        )
        .optional()
        .with_context(|| format!("check bead existence for {}", bead_id.as_str()))?;
    if exists.is_some() {
        Ok(())
    } else {
        bail!("bead {} does not exist", bead_id.as_str())
    }
}

fn ensure_run_exists(tx: &Transaction<'_>, run_id: &RunId) -> Result<()> {
    let exists = tx
        .query_row(
            "SELECT 1 FROM task_runs WHERE id = ?1",
            [run_id.as_str()],
            |_| Ok(()),
        )
        .optional()
        .with_context(|| format!("check run existence for {}", run_id.as_str()))?;
    if exists.is_some() {
        Ok(())
    } else {
        bail!("run {} does not exist", run_id.as_str())
    }
}

fn ensure_run_belongs_to_bead(
    tx: &Transaction<'_>,
    run_id: &RunId,
    bead_id: &BeadId,
) -> Result<()> {
    let run_bead_id = tx
        .query_row(
            "SELECT bead_id FROM task_runs WHERE id = ?1",
            [run_id.as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .with_context(|| format!("query bead linkage for run {}", run_id.as_str()))?;
    match run_bead_id.as_deref() {
        Some(found) if found == bead_id.as_str() => Ok(()),
        Some(found) => bail!(
            "run {} belongs to bead {}, not {}",
            run_id.as_str(),
            found,
            bead_id.as_str()
        ),
        None => bail!("run {} does not exist", run_id.as_str()),
    }
}

fn ensure_session_belongs_to_run(
    tx: &Transaction<'_>,
    session_id: &SessionId,
    run_id: &RunId,
) -> Result<()> {
    let session_run_id = tx
        .query_row(
            "SELECT run_id FROM claude_sessions WHERE id = ?1",
            [session_id.as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .with_context(|| format!("query run linkage for session {}", session_id.as_str()))?;
    match session_run_id.as_deref() {
        Some(found) if found == run_id.as_str() => Ok(()),
        Some(found) => bail!(
            "session {} belongs to run {}, not {}",
            session_id.as_str(),
            found,
            run_id.as_str()
        ),
        None => bail!("session {} does not exist", session_id.as_str()),
    }
}

fn list_active_reservations_tx(
    tx: &Transaction<'_>,
    now: &chrono::DateTime<Utc>,
) -> Result<Vec<ReservationRecord>> {
    let mut stmt = tx
        .prepare(
            "SELECT id, bead_id, run_id, path_pattern, exclusive, reason, expires_at, released_at \
             FROM reservations \
             WHERE released_at IS NULL \
               AND expires_at > ?1 \
             ORDER BY bead_id ASC, id ASC",
        )
        .context("prepare active reservations tx query")?;
    let now = timestamp_string(now);
    let rows = stmt
        .query_map([&now], raw_reservation_row)
        .context("query active reservations in tx")?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("collect active reservations in tx")?
        .into_iter()
        .map(raw_reservation_into_record)
        .collect()
}

fn active_leader_lease_tx(
    tx: &Transaction<'_>,
    now: &chrono::DateTime<Utc>,
) -> Result<Option<LeaderLeaseRecord>> {
    let mut stmt = tx
        .prepare(
            "SELECT owner_label, run_id, acquired_at, heartbeat_at, expires_at, released_at \
             FROM leader_leases \
             WHERE slot = 1 AND released_at IS NULL AND expires_at > ?1",
        )
        .context("prepare active leader lease tx query")?;
    let raw = stmt
        .query_row([timestamp_string(now)], raw_leader_lease_row)
        .optional()
        .context("query active leader lease tx")?;
    raw.map(raw_leader_lease_into_record).transpose()
}

fn list_runs_by_status_tx(tx: &Transaction<'_>, status: RunStatus) -> Result<Vec<TaskRunRecord>> {
    let mut stmt = tx
        .prepare(
            "SELECT id, bead_id, attempt_no, status, failure_class, failure_detail, started_at, ended_at, session_count, checkpoint_count, last_checkpoint_id, activity, last_activity_at, escalation_tier \
             FROM task_runs WHERE status = ?1 ORDER BY started_at ASC, id ASC",
        )
        .context("prepare runs by status query")?;
    let rows = stmt
        .query_map([encode_run_status(status)], raw_task_run_row)
        .context("query runs by status")?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("collect runs by status")?
        .into_iter()
        .map(raw_task_run_into_record)
        .collect()
}

fn list_expired_unreleased_reservations_tx(
    tx: &Transaction<'_>,
    now: &chrono::DateTime<Utc>,
) -> Result<Vec<ReservationRecord>> {
    let mut stmt = tx
        .prepare(
            "SELECT id, bead_id, run_id, path_pattern, exclusive, reason, expires_at, released_at \
             FROM reservations \
             WHERE released_at IS NULL \
               AND expires_at <= ?1 \
             ORDER BY bead_id ASC, id ASC",
        )
        .context("prepare expired reservation query")?;
    let now = timestamp_string(now);
    let rows = stmt
        .query_map([&now], raw_reservation_row)
        .context("query expired reservations")?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("collect expired reservations")?
        .into_iter()
        .map(raw_reservation_into_record)
        .collect()
}

fn list_releasable_reservations_tx(
    tx: &Transaction<'_>,
    bead_id: &BeadId,
    run_id: Option<&RunId>,
    path_patterns: Option<&[String]>,
) -> Result<Vec<ReservationRecord>> {
    let mut reservations = list_all_reservations_for_bead_tx(tx, bead_id)?;
    reservations.retain(|record| {
        record.released_at.is_none()
            && run_id.is_none_or(|expected| record.run_id.as_ref() == Some(expected))
            && path_patterns.is_none_or(|patterns| {
                patterns
                    .iter()
                    .any(|pattern| pattern == &record.path_pattern)
            })
    });
    Ok(reservations)
}

fn list_all_reservations_for_bead_tx(
    tx: &Transaction<'_>,
    bead_id: &BeadId,
) -> Result<Vec<ReservationRecord>> {
    let mut stmt = tx
        .prepare(
            "SELECT id, bead_id, run_id, path_pattern, exclusive, reason, expires_at, released_at \
             FROM reservations \
             WHERE bead_id = ?1 \
             ORDER BY id ASC",
        )
        .context("prepare bead reservations tx query")?;
    let rows = stmt
        .query_map([bead_id.as_str()], raw_reservation_row)
        .with_context(|| format!("query reservations in tx for {}", bead_id.as_str()))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("collect bead reservations in tx")?
        .into_iter()
        .map(raw_reservation_into_record)
        .collect()
}

fn mark_reservation_released_tx(
    tx: &Transaction<'_>,
    reservation_id: i64,
    released_at: &chrono::DateTime<Utc>,
) -> Result<()> {
    tx.execute(
        "UPDATE reservations SET released_at = ?2 WHERE id = ?1 AND released_at IS NULL",
        params![reservation_id, timestamp_string(released_at)],
    )
    .with_context(|| format!("release reservation {reservation_id}"))?;
    Ok(())
}

fn active_declared_paths_tx(
    tx: &Transaction<'_>,
    bead_id: &BeadId,
    now: &chrono::DateTime<Utc>,
) -> Result<Vec<String>> {
    let mut records = list_all_reservations_for_bead_tx(tx, bead_id)?;
    records.retain(|record| record.released_at.is_none() && record.expires_at > *now);
    Ok(records
        .into_iter()
        .map(|record| record.path_pattern)
        .collect())
}

fn refresh_declared_paths_for_beads_tx(
    tx: &Transaction<'_>,
    bead_ids: Vec<BeadId>,
    now: &chrono::DateTime<Utc>,
) -> Result<()> {
    let mut unique = bead_ids;
    unique.sort();
    unique.dedup();
    for bead_id in unique {
        let paths = active_declared_paths_tx(tx, &bead_id, now)?;
        set_declared_paths_tx(tx, &bead_id, None, &paths)?;
    }
    Ok(())
}

fn set_declared_paths_tx(
    tx: &Transaction<'_>,
    bead_id: &BeadId,
    run_id: Option<&RunId>,
    declared_paths: &[String],
) -> Result<()> {
    let runtime_updated_at = now_timestamp_string();
    tx.execute(
        "INSERT INTO bead_runtime(\
            bead_id, grove_status, declared_paths_json, metadata_json, last_run_id, retry_after,\
            last_failure_class, last_failure_detail, circuit_breaker_json, runtime_updated_at\
         ) VALUES (\
            ?1, COALESCE((SELECT grove_status FROM bead_runtime WHERE bead_id = ?1), 'Idle'), ?2,\
            COALESCE((SELECT metadata_json FROM bead_runtime WHERE bead_id = ?1), '{}'),\
            COALESCE(?3, (SELECT last_run_id FROM bead_runtime WHERE bead_id = ?1)),\
            COALESCE((SELECT retry_after FROM bead_runtime WHERE bead_id = ?1), NULL),\
            COALESCE((SELECT last_failure_class FROM bead_runtime WHERE bead_id = ?1), NULL),\
            COALESCE((SELECT last_failure_detail FROM bead_runtime WHERE bead_id = ?1), NULL),\
            COALESCE((SELECT circuit_breaker_json FROM bead_runtime WHERE bead_id = ?1), NULL),\
            ?4\
         )\
         ON CONFLICT(bead_id) DO UPDATE SET \
            declared_paths_json = excluded.declared_paths_json,\
            last_run_id = COALESCE(excluded.last_run_id, bead_runtime.last_run_id),\
            runtime_updated_at = excluded.runtime_updated_at",
        params![
            bead_id.as_str(),
            serde_json::to_string(declared_paths).context("serialize declared paths")?,
            run_id.map(RunId::as_str),
            runtime_updated_at,
        ],
    )
    .with_context(|| format!("update declared paths for {}", bead_id.as_str()))?;
    Ok(())
}

fn insert_event_log_tx(
    tx: &Transaction<'_>,
    kind: EventKind,
    bead_id: Option<&BeadId>,
    run_id: Option<&RunId>,
    session_id: Option<&SessionId>,
    payload: &serde_json::Value,
    created_at: &chrono::DateTime<Utc>,
) -> Result<()> {
    let input = EventLogInput {
        kind,
        bead_id: bead_id.cloned(),
        run_id: run_id.cloned(),
        session_id: session_id.cloned(),
        payload: payload.clone(),
        created_at: *created_at,
        observability: EventObservability::default(),
    };
    insert_event_log_with_observability_tx(tx, &input)
}

fn insert_event_log_with_observability_tx(
    tx: &Transaction<'_>,
    input: &EventLogInput,
) -> Result<()> {
    let error_json = input
        .observability
        .error
        .as_ref()
        .and_then(|e| serde_json::to_string(e).ok());
    let context_snapshot_json = input
        .observability
        .context_snapshot
        .as_ref()
        .and_then(|cs| serde_json::to_string(cs).ok());

    tx.execute(
        "INSERT INTO event_log(kind, bead_id, run_id, session_id, payload_json, created_at, \
            correlation_id, operation, outcome, duration_ms, error_json, context_snapshot_json) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            encode_event_kind(input.kind),
            input.bead_id.as_ref().map(BeadId::as_str),
            input.run_id.as_ref().map(RunId::as_str),
            input.session_id.as_ref().map(SessionId::as_str),
            input.payload.to_string(),
            timestamp_string(&input.created_at),
            input.observability.correlation_id.as_deref(),
            input.observability.operation.as_deref(),
            input.observability.outcome.map(encode_event_outcome),
            input.observability.duration_ms.map(|ms| ms as i64),
            error_json,
            context_snapshot_json,
        ],
    )
    .with_context(|| format!("insert event log {:?}", input.kind))?;
    Ok(())
}

fn encode_event_kind(kind: EventKind) -> &'static str {
    match kind {
        EventKind::BeadCacheSynced => "BeadCacheSynced",
        EventKind::DependencySnapshotSynced => "DependencySnapshotSynced",
        EventKind::GroveStatusUpdated => "GroveStatusUpdated",
        EventKind::RunStarted => "RunStarted",
        EventKind::RunCheckpointed => "RunCheckpointed",
        EventKind::RunSucceeded => "RunSucceeded",
        EventKind::RunFailed => "RunFailed",
        EventKind::SessionStarted => "SessionStarted",
        EventKind::SessionCheckpointed => "SessionCheckpointed",
        EventKind::SessionSucceeded => "SessionSucceeded",
        EventKind::SessionFailed => "SessionFailed",
        EventKind::HandoffWritten => "HandoffWritten",
        EventKind::ReservationGranted => "ReservationGranted",
        EventKind::ReservationConflictDetected => "ReservationConflictDetected",
        EventKind::ReservationExpired => "ReservationExpired",
        EventKind::RecoveryActionTaken => "RecoveryActionTaken",
        EventKind::LeaseAcquired => "LeaseAcquired",
        EventKind::LeaseHeartbeat => "LeaseHeartbeat",
        EventKind::LeaseReleased => "LeaseReleased",
        EventKind::ShutdownRequested => "ShutdownRequested",
        EventKind::SessionTerminationRequested => "SessionTerminationRequested",
        EventKind::SessionTerminationForced => "SessionTerminationForced",
        EventKind::CoordinatorStopped => "CoordinatorStopped",
        EventKind::ArchiveIngested => "ArchiveIngested",
        EventKind::PlaybookBulletAdded => "PlaybookBulletAdded",
        EventKind::PlaybookBulletPromoted => "PlaybookBulletPromoted",
        EventKind::PlaybookBulletDeprecated => "PlaybookBulletDeprecated",
        EventKind::BrMirrorRequested => "BrMirrorRequested",
        EventKind::BrMirrorSucceeded => "BrMirrorSucceeded",
        EventKind::BrMirrorFailed => "BrMirrorFailed",
        EventKind::ReactionInvoked => "ReactionInvoked",
        EventKind::EscalationTierChanged => "EscalationTierChanged",
        EventKind::EscalationTierReset => "EscalationTierReset",
        EventKind::ActivityStateChanged => "ActivityStateChanged",
        EventKind::RecoveryCapsuleCreated => "RecoveryCapsuleCreated",
    }
}

fn is_run_terminal_tx(tx: &Transaction<'_>, run_id: &RunId) -> Result<bool> {
    let status = tx
        .query_row(
            "SELECT status FROM task_runs WHERE id = ?1",
            [run_id.as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .with_context(|| format!("query run status for {}", run_id.as_str()))?;
    Ok(status
        .as_deref()
        .map(parse_run_status)
        .transpose()?
        .is_some_and(|status| status != RunStatus::Active))
}

fn run_status_for_event_tx(tx: &Transaction<'_>, run_id: &RunId) -> Result<String> {
    tx.query_row(
        "SELECT status FROM task_runs WHERE id = ?1",
        [run_id.as_str()],
        |row| row.get::<_, String>(0),
    )
    .with_context(|| format!("query run status text for {}", run_id.as_str()))
}

fn session_failure_class(session: &ClaudeSessionRecord) -> Option<FailureClass> {
    match session.status {
        SessionStatus::TimedOut => Some(FailureClass::Timeout),
        SessionStatus::RateLimited => Some(FailureClass::RateLimit),
        SessionStatus::PermissionDenied => Some(FailureClass::PermissionDenied),
        SessionStatus::Crashed => Some(FailureClass::ClaudeCrashed),
        SessionStatus::UnknownFailure if session.stop_reason == Some(StopReason::Kill) => {
            Some(FailureClass::Interrupted)
        }
        SessionStatus::UnknownFailure => Some(FailureClass::Unknown),
        SessionStatus::Starting
        | SessionStatus::Running
        | SessionStatus::Checkpointed
        | SessionStatus::Completed => None,
    }
}

fn upsert_bead_runtime_tx(
    tx: &Transaction<'_>,
    bead_id: &BeadId,
    grove_status: Option<GroveBeadStatus>,
    declared_paths: Option<Vec<String>>,
    last_run_id: Option<Option<RunId>>,
    retry_after: Option<Option<chrono::DateTime<Utc>>>,
    last_failure_class: Option<Option<FailureClass>>,
    last_failure_detail: Option<Option<String>>,
    circuit_breaker_state: Option<Option<CircuitBreakerState>>,
    runtime_updated_at: &chrono::DateTime<Utc>,
) -> Result<()> {
    let declared_paths_json = declared_paths
        .map(|paths| serde_json::to_string(&paths).context("serialize bead runtime declared paths"))
        .transpose()?;
    let last_run_id = last_run_id.flatten().map(|value| value.to_string());
    let retry_after = retry_after.flatten().map(|value| timestamp_string(&value));
    let last_failure_class = last_failure_class.flatten().map(encode_failure_class);
    let last_failure_detail = last_failure_detail.flatten();
    let circuit_breaker_json = circuit_breaker_state
        .flatten()
        .map(|state| serde_json::to_string(&state).context("serialize circuit breaker state"))
        .transpose()?;
    let runtime_updated_at = timestamp_string(runtime_updated_at);

    tx.execute(
        "INSERT INTO bead_runtime(\
            bead_id, grove_status, declared_paths_json, metadata_json, last_run_id, retry_after,\
            last_failure_class, last_failure_detail, circuit_breaker_json, runtime_updated_at\
         ) VALUES (\
            ?1, ?2, COALESCE(?3, (SELECT declared_paths_json FROM bead_runtime WHERE bead_id = ?1), '[]'),\
            COALESCE((SELECT metadata_json FROM bead_runtime WHERE bead_id = ?1), '{}'), ?4, ?5, ?6, ?7,\
            COALESCE(?8, (SELECT circuit_breaker_json FROM bead_runtime WHERE bead_id = ?1), NULL), ?9\
         ) \
         ON CONFLICT(bead_id) DO UPDATE SET \
            grove_status = COALESCE(excluded.grove_status, bead_runtime.grove_status),\
            declared_paths_json = COALESCE(?3, bead_runtime.declared_paths_json),\
            last_run_id = excluded.last_run_id,\
            retry_after = excluded.retry_after,\
            last_failure_class = excluded.last_failure_class,\
            last_failure_detail = excluded.last_failure_detail,\
            circuit_breaker_json = excluded.circuit_breaker_json,\
            runtime_updated_at = excluded.runtime_updated_at",
        params![
            bead_id.as_str(),
            grove_status.map(encode_grove_bead_status),
            declared_paths_json,
            last_run_id,
            retry_after,
            last_failure_class,
            last_failure_detail,
            circuit_breaker_json,
            runtime_updated_at,
        ],
    )
    .with_context(|| format!("upsert bead runtime for {}", bead_id.as_str()))?;
    Ok(())
}

fn conflicts_for_request(
    bead_id: &BeadId,
    _run_id: Option<&RunId>,
    request: &ReservationRequest<'_>,
    active: &[ReservationRecord],
) -> Vec<ReservationConflict> {
    active
        .iter()
        .filter(|record| record.bead_id != *bead_id)
        .filter(|record| {
            request.mode == ReservationMode::Exclusive || record.mode == ReservationMode::Exclusive
        })
        .filter(|record| reservation_patterns_overlap(request.path_pattern, &record.path_pattern))
        .map(|record| ReservationConflict {
            requested_by_bead: bead_id.clone(),
            conflicting_bead: record.bead_id.clone(),
            requested_pattern: request.path_pattern.to_owned(),
            held_pattern: record.path_pattern.clone(),
            conflicting_run_id: record.run_id.clone(),
        })
        .collect()
}

pub fn reservation_patterns_overlap(left: &str, right: &str) -> bool {
    let left = normalize_reservation_pattern(left);
    let right = normalize_reservation_pattern(right);

    if left == right {
        return true;
    }

    pattern_matches_path(left, right) || pattern_matches_path(right, left)
}

fn normalize_reservation_pattern(pattern: &str) -> &str {
    pattern.trim().trim_end_matches('/')
}

fn pattern_matches_path(pattern: &str, candidate: &str) -> bool {
    Pattern::new(pattern).is_ok_and(|glob| {
        glob.matches_with(
            candidate,
            MatchOptions {
                require_literal_separator: true,
                ..MatchOptions::new()
            },
        )
    })
}

fn parse_json<T: serde::de::DeserializeOwned>(text: &str, label: &str) -> Result<T> {
    serde_json::from_str(text).with_context(|| format!("parse {label} JSON"))
}

fn raw_issue_timestamp(raw_json: &Value, field: &str) -> Result<Option<Timestamp>> {
    raw_json
        .get(field)
        .and_then(Value::as_str)
        .map(parse_timestamp)
        .transpose()
        .with_context(|| format!("parse {field} from raw bead JSON"))
}

fn parse_timestamp(text: &str) -> Result<Timestamp> {
    chrono::DateTime::parse_from_rfc3339(text)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .or_else(|_| {
            NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S")
                .map(|timestamp| Utc.from_utc_datetime(&timestamp))
        })
        .with_context(|| format!("parse timestamp {text}"))
}

fn now_timestamp_string() -> String {
    timestamp_string(&Utc::now())
}

fn timestamp_string(timestamp: &chrono::DateTime<Utc>) -> String {
    timestamp.to_rfc3339()
}

fn encode_reservation_mode(mode: ReservationMode) -> &'static str {
    match mode {
        ReservationMode::Shared => "shared",
        ReservationMode::Exclusive => "exclusive",
    }
}

fn parse_bead_priority(value: i64) -> Result<BeadPriority> {
    match value {
        0 => Ok(BeadPriority::P0),
        1 => Ok(BeadPriority::P1),
        2 => Ok(BeadPriority::P2),
        3 => Ok(BeadPriority::P3),
        4 => Ok(BeadPriority::P4),
        _ => bail!("unsupported bead priority value {value}"),
    }
}

fn bead_priority_to_db(priority: BeadPriority) -> i64 {
    match priority {
        BeadPriority::P0 => 0,
        BeadPriority::P1 => 1,
        BeadPriority::P2 => 2,
        BeadPriority::P3 => 3,
        BeadPriority::P4 => 4,
    }
}

fn encode_grove_bead_status(status: GroveBeadStatus) -> &'static str {
    match status {
        GroveBeadStatus::Idle => "Idle",
        GroveBeadStatus::Ready => "Ready",
        GroveBeadStatus::Running => "Running",
        GroveBeadStatus::Checkpointed => "Checkpointed",
        GroveBeadStatus::WaitingToRetry => "WaitingToRetry",
        GroveBeadStatus::Succeeded => "Succeeded",
        GroveBeadStatus::Failed => "Failed",
    }
}

fn encode_agent_activity(activity: grove_types::AgentActivity) -> &'static str {
    match activity {
        grove_types::AgentActivity::Active => "Active",
        grove_types::AgentActivity::Ready => "Ready",
        grove_types::AgentActivity::Idle => "Idle",
        grove_types::AgentActivity::Blocked => "Blocked",
        grove_types::AgentActivity::Exited => "Exited",
    }
}

fn encode_escalation_tier(tier: grove_types::EscalationTier) -> &'static str {
    match tier {
        grove_types::EscalationTier::FirstAttempt => "FirstAttempt",
        grove_types::EscalationTier::SecondAttempt => "SecondAttempt",
        grove_types::EscalationTier::ThirdAttempt => "ThirdAttempt",
        grove_types::EscalationTier::FinalAttempt => "FinalAttempt",
        grove_types::EscalationTier::GiveUp => "GiveUp",
    }
}

fn encode_failure_class(class: FailureClass) -> &'static str {
    match class {
        FailureClass::Timeout => "Timeout",
        FailureClass::RateLimit => "RateLimit",
        FailureClass::PermissionDenied => "PermissionDenied",
        FailureClass::CircuitOpen => "CircuitOpen",
        FailureClass::NoProgress => "NoProgress",
        FailureClass::RepeatedError => "RepeatedError",
        FailureClass::ProtocolMalformed => "ProtocolMalformed",
        FailureClass::ClaudeCrashed => "ClaudeCrashed",
        FailureClass::BrMirrorFailed => "BrMirrorFailed",
        FailureClass::Interrupted => "Interrupted",
        FailureClass::Unknown => "Unknown",
    }
}

fn mirror_status_from_str(status: &str) -> Option<MirrorStatus> {
    match status {
        "pending" => Some(MirrorStatus::Pending),
        "in_progress" => Some(MirrorStatus::InProgress),
        "succeeded" => Some(MirrorStatus::Succeeded),
        "failed" => Some(MirrorStatus::Failed),
        _ => None,
    }
}

fn encode_run_status(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Active => "Active",
        RunStatus::WaitingToRetry => "WaitingToRetry",
        RunStatus::Checkpointed => "Checkpointed",
        RunStatus::Succeeded => "Succeeded",
        RunStatus::Failed => "Failed",
    }
}

fn encode_session_status(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Starting => "Starting",
        SessionStatus::Running => "Running",
        SessionStatus::Checkpointed => "Checkpointed",
        SessionStatus::Completed => "Completed",
        SessionStatus::TimedOut => "TimedOut",
        SessionStatus::RateLimited => "RateLimited",
        SessionStatus::PermissionDenied => "PermissionDenied",
        SessionStatus::Crashed => "Crashed",
        SessionStatus::UnknownFailure => "UnknownFailure",
    }
}

fn encode_stop_reason(reason: StopReason) -> &'static str {
    match reason {
        StopReason::Exit => "Exit",
        StopReason::Checkpoint => "Checkpoint",
        StopReason::Timeout => "Timeout",
        StopReason::RateLimit => "RateLimit",
        StopReason::PermissionDenied => "PermissionDenied",
        StopReason::Crash => "Crash",
        StopReason::Kill => "Kill",
        StopReason::Unknown => "Unknown",
    }
}

fn parse_grove_bead_status(text: &str) -> Result<GroveBeadStatus> {
    match normalize_enum_token(text).as_str() {
        "idle" => Ok(GroveBeadStatus::Idle),
        "ready" => Ok(GroveBeadStatus::Ready),
        "running" => Ok(GroveBeadStatus::Running),
        "checkpointed" => Ok(GroveBeadStatus::Checkpointed),
        "waitingtoretry" => Ok(GroveBeadStatus::WaitingToRetry),
        "succeeded" => Ok(GroveBeadStatus::Succeeded),
        "failed" => Ok(GroveBeadStatus::Failed),
        _ => bail!("unsupported grove bead status {text}"),
    }
}

fn parse_agent_activity(text: &str) -> Result<grove_types::AgentActivity> {
    match normalize_enum_token(text).as_str() {
        "active" => Ok(grove_types::AgentActivity::Active),
        "ready" => Ok(grove_types::AgentActivity::Ready),
        "idle" => Ok(grove_types::AgentActivity::Idle),
        "blocked" => Ok(grove_types::AgentActivity::Blocked),
        "exited" => Ok(grove_types::AgentActivity::Exited),
        _ => bail!("unsupported agent activity {text}"),
    }
}

fn parse_escalation_tier(text: &str) -> Result<grove_types::EscalationTier> {
    match normalize_enum_token(text).as_str() {
        "firstattempt" => Ok(grove_types::EscalationTier::FirstAttempt),
        "secondattempt" => Ok(grove_types::EscalationTier::SecondAttempt),
        "thirdattempt" => Ok(grove_types::EscalationTier::ThirdAttempt),
        "finalattempt" => Ok(grove_types::EscalationTier::FinalAttempt),
        "giveup" => Ok(grove_types::EscalationTier::GiveUp),
        _ => bail!("unsupported escalation tier {text}"),
    }
}

fn parse_failure_class(text: &str) -> Result<FailureClass> {
    match normalize_enum_token(text).as_str() {
        "timeout" => Ok(FailureClass::Timeout),
        "ratelimit" => Ok(FailureClass::RateLimit),
        "permissiondenied" => Ok(FailureClass::PermissionDenied),
        "circuitopen" => Ok(FailureClass::CircuitOpen),
        "noprogress" => Ok(FailureClass::NoProgress),
        "repeatederror" => Ok(FailureClass::RepeatedError),
        "protocolmalformed" => Ok(FailureClass::ProtocolMalformed),
        "claudecrashed" => Ok(FailureClass::ClaudeCrashed),
        "brmirrorfailed" => Ok(FailureClass::BrMirrorFailed),
        "interrupted" => Ok(FailureClass::Interrupted),
        "unknown" => Ok(FailureClass::Unknown),
        _ => bail!("unsupported failure class {text}"),
    }
}

fn parse_run_status(text: &str) -> Result<RunStatus> {
    match normalize_enum_token(text).as_str() {
        "active" => Ok(RunStatus::Active),
        "waitingtoretry" => Ok(RunStatus::WaitingToRetry),
        "checkpointed" => Ok(RunStatus::Checkpointed),
        "succeeded" => Ok(RunStatus::Succeeded),
        "failed" => Ok(RunStatus::Failed),
        _ => bail!("unsupported run status {text}"),
    }
}

fn parse_session_status(text: &str) -> Result<SessionStatus> {
    match normalize_enum_token(text).as_str() {
        "starting" => Ok(SessionStatus::Starting),
        "running" => Ok(SessionStatus::Running),
        "checkpointed" => Ok(SessionStatus::Checkpointed),
        "completed" => Ok(SessionStatus::Completed),
        "timedout" => Ok(SessionStatus::TimedOut),
        "ratelimited" => Ok(SessionStatus::RateLimited),
        "permissiondenied" => Ok(SessionStatus::PermissionDenied),
        "crashed" => Ok(SessionStatus::Crashed),
        "unknownfailure" => Ok(SessionStatus::UnknownFailure),
        _ => bail!("unsupported session status {text}"),
    }
}

fn parse_stop_reason(text: &str) -> Result<StopReason> {
    match normalize_enum_token(text).as_str() {
        "exit" => Ok(StopReason::Exit),
        "checkpoint" => Ok(StopReason::Checkpoint),
        "timeout" => Ok(StopReason::Timeout),
        "ratelimit" => Ok(StopReason::RateLimit),
        "permissiondenied" => Ok(StopReason::PermissionDenied),
        "crash" => Ok(StopReason::Crash),
        "kill" => Ok(StopReason::Kill),
        "unknown" => Ok(StopReason::Unknown),
        _ => bail!("unsupported stop reason {text}"),
    }
}

fn parse_event_kind(text: &str) -> Result<EventKind> {
    match normalize_enum_token(text).as_str() {
        "beadcachesynced" => Ok(EventKind::BeadCacheSynced),
        "dependencysnapshotsynced" => Ok(EventKind::DependencySnapshotSynced),
        "grovestatusupdated" => Ok(EventKind::GroveStatusUpdated),
        "runstarted" => Ok(EventKind::RunStarted),
        "runcheckpointed" => Ok(EventKind::RunCheckpointed),
        "runsucceeded" => Ok(EventKind::RunSucceeded),
        "runfailed" => Ok(EventKind::RunFailed),
        "sessionstarted" => Ok(EventKind::SessionStarted),
        "sessioncheckpointed" => Ok(EventKind::SessionCheckpointed),
        "sessionsucceeded" => Ok(EventKind::SessionSucceeded),
        "sessionfailed" => Ok(EventKind::SessionFailed),
        "handoffwritten" => Ok(EventKind::HandoffWritten),
        "reservationgranted" => Ok(EventKind::ReservationGranted),
        "reservationconflictdetected" => Ok(EventKind::ReservationConflictDetected),
        "reservationexpired" => Ok(EventKind::ReservationExpired),
        "recoveryactiontaken" => Ok(EventKind::RecoveryActionTaken),
        "leaseacquired" => Ok(EventKind::LeaseAcquired),
        "leaseheartbeat" => Ok(EventKind::LeaseHeartbeat),
        "leasereleased" => Ok(EventKind::LeaseReleased),
        "shutdownrequested" => Ok(EventKind::ShutdownRequested),
        "sessionterminationrequested" => Ok(EventKind::SessionTerminationRequested),
        "sessionterminationforced" => Ok(EventKind::SessionTerminationForced),
        "coordinatorstopped" => Ok(EventKind::CoordinatorStopped),
        "archiveingested" => Ok(EventKind::ArchiveIngested),
        "playbookbulletadded" => Ok(EventKind::PlaybookBulletAdded),
        "playbookbulletpromoted" => Ok(EventKind::PlaybookBulletPromoted),
        "playbookbulletdeprecated" => Ok(EventKind::PlaybookBulletDeprecated),
        "brmirrorrequested" => Ok(EventKind::BrMirrorRequested),
        "brmirrorsucceeded" => Ok(EventKind::BrMirrorSucceeded),
        "brmirrorfailed" => Ok(EventKind::BrMirrorFailed),
        "reactioninvoked" => Ok(EventKind::ReactionInvoked),
        "escalationtierchanged" => Ok(EventKind::EscalationTierChanged),
        "escalationtierreset" => Ok(EventKind::EscalationTierReset),
        "activitystatechanged" => Ok(EventKind::ActivityStateChanged),
        "recoverycapsulecreated" => Ok(EventKind::RecoveryCapsuleCreated),
        _ => bail!("unsupported event kind {text}"),
    }
}

fn parse_event_outcome(text: &str) -> Option<EventOutcome> {
    match normalize_enum_token(text).as_str() {
        "success" => Some(EventOutcome::Success),
        "failure" => Some(EventOutcome::Failure),
        "partial" => Some(EventOutcome::Partial),
        _ => None,
    }
}

fn encode_event_outcome(outcome: EventOutcome) -> &'static str {
    match outcome {
        EventOutcome::Success => "Success",
        EventOutcome::Failure => "Failure",
        EventOutcome::Partial => "Partial",
    }
}

fn normalize_enum_token(text: &str) -> String {
    text.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::reservation_patterns_overlap;
    use anyhow::Result;
    use camino::Utf8PathBuf;
    use chrono::Utc;
    use grove_br::{
        sync_bead_cache, BeadCacheStore, BrCapability, BrClient, BrDependencySnapshot, BrError,
        BrIssueDetail, BrIssueSummary, BrVersion,
    };
    use grove_types::{
        BeadId, BeadPriority, CheckpointId, CheckpointPayload, CircuitBreakerState,
        ClaudeSessionRecord, EventKind, FailureClass, HandoffRecord, PromptId,
        RecoveryCapsuleOutcome, ReservationMode, RunId, RunStatus, SessionId, SessionStatus,
        StopReason, Timestamp,
    };
    use rusqlite::OptionalExtension;
    use serde_json::json;
    use std::collections::BTreeMap;
    use tempfile::tempdir;

    use super::{
        CachedBeadState, Database, GroveBeadStatus, RunFinishInput, RunStartInput,
        SessionCheckpointInput,
    };
    use crate::MigrationState;

    #[test]
    fn open_creates_database_parent_directory() -> Result<()> {
        let dir = tempdir()?;
        let db_path = Utf8PathBuf::from_path_buf(dir.path().join("nested/.grove/grove.db"))
            .map_err(|_| anyhow::anyhow!("temp path was not valid UTF-8"))?;

        let _db = Database::open(&db_path)?;

        assert!(db_path.exists());
        Ok(())
    }

    #[test]
    fn migrate_applies_manifest_once() -> Result<()> {
        let dir = tempdir()?;
        let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
            .map_err(|_| anyhow::anyhow!("temp path was not valid UTF-8"))?;
        let mut db = Database::open(&db_path)?;

        db.migrate()?;
        db.migrate()?;

        let migrations = db.applied_migrations()?;
        assert_eq!(migrations.len(), 11);
        assert_eq!(
            migrations[0],
            MigrationState {
                version: 1,
                name: "0001_init.sql".into(),
            }
        );
        assert_eq!(
            migrations[1],
            MigrationState {
                version: 2,
                name: "0002_prompt_manifest_columns.sql".into(),
            }
        );
        assert_eq!(
            migrations[2],
            MigrationState {
                version: 3,
                name: "0003_leader_lease.sql".into(),
            }
        );
        assert_eq!(
            migrations[3],
            MigrationState {
                version: 4,
                name: "0004_mirror_outbox.sql".into(),
            }
        );
        assert_eq!(
            migrations[4],
            MigrationState {
                version: 5,
                name: "0005_operational_schema.sql".into(),
            }
        );
        assert_eq!(
            migrations[5],
            MigrationState {
                version: 6,
                name: "0006_observability.sql".into(),
            }
        );
        assert_eq!(
            migrations[6],
            MigrationState {
                version: 7,
                name: "0007_archive_fts.sql".into(),
            }
        );
        assert_eq!(
            migrations[7],
            MigrationState {
                version: 8,
                name: "0008_archive_watermarks.sql".into(),
            }
        );
        assert_eq!(
            migrations[8],
            MigrationState {
                version: 9,
                name: "0009_playbook.sql".into(),
            }
        );
        assert_eq!(
            migrations[9],
            MigrationState {
                version: 10,
                name: "0010_activity_state.sql".into(),
            }
        );
        assert_eq!(
            migrations[10],
            MigrationState {
                version: 11,
                name: "0011_circuit_breaker_state.sql".into(),
            }
        );
        Ok(())
    }

    #[test]
    fn migrate_creates_runtime_tables() -> Result<()> {
        let dir = tempdir()?;
        let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
            .map_err(|_| anyhow::anyhow!("temp path was not valid UTF-8"))?;
        let mut db = Database::open(&db_path)?;

        db.migrate()?;

        for table in [
            "_migrations",
            "bead_cache",
            "bead_runtime",
            "bead_dependencies",
            "task_runs",
            "claude_sessions",
            "checkpoints",
            "handoffs",
            "reservations",
            "event_log",
            "leader_leases",
        ] {
            let exists: Option<String> = db
                .connection()
                .query_row(
                    "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    [table],
                    |row| row.get(0),
                )
                .optional()?;
            assert_eq!(exists.as_deref(), Some(table));
        }

        Ok(())
    }

    #[test]
    fn with_tx_commits_changes() -> Result<()> {
        let dir = tempdir()?;
        let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
            .map_err(|_| anyhow::anyhow!("temp path was not valid UTF-8"))?;
        let mut db = Database::open(&db_path)?;
        db.migrate()?;

        db.with_tx(|tx| {
            tx.execute(
                "INSERT INTO bead_cache(\
                    bead_id, title, description, priority, issue_type, status, assignee,\
                    labels_json, parent_ids_json, dependency_ids_json, dependent_ids_json,\
                    raw_json, synced_at\
                ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, NULL, '[]', '[]', '[]', '[]', ?6, CURRENT_TIMESTAMP)",
                (
                    "grove-123",
                    "Example bead",
                    0,
                    "task",
                    "open",
                    "{}",
                ),
            )?;
            Ok(())
        })?;

        let count: i64 =
            db.connection()
                .query_row("SELECT COUNT(*) FROM bead_cache", [], |row| row.get(0))?;
        assert_eq!(count, 1);
        Ok(())
    }

    #[test]
    fn sync_bead_cache_populates_database_records_and_runtime_state() -> Result<()> {
        let dir = tempdir()?;
        let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
            .map_err(|_| anyhow::anyhow!("temp path was not valid UTF-8"))?;
        let mut db = Database::open(&db_path)?;
        db.migrate()?;

        let bead = sample_issue(
            "grove-1j9.5.7",
            "kernel services",
            vec![BeadId::new("grove-1j9.5.4")],
            vec![BeadId::new("grove-1j9.5.8")],
        )?;
        let br = FakeBrClient {
            ready: vec![bead.clone()],
            list_open: vec![bead.clone()],
            dep_snapshots: BTreeMap::from([(
                bead.id.as_str().to_owned(),
                bead.dependency_snapshot(),
            )]),
        };

        let first = sync_bead_cache(&br, &mut db)?;
        let second = sync_bead_cache(&br, &mut db)?;

        assert_eq!(first.beads_added, 1, "first sync result: {first:?}");
        assert_eq!(second.beads_updated, 1);
        assert!(first.errors.is_empty());
        assert!(second.errors.is_empty());

        let cached = db.list_cached_beads()?;
        assert_eq!(
            cached,
            vec![CachedBeadState {
                bead_id: bead.id.clone(),
                grove_status: Some(GroveBeadStatus::Ready),
            }]
        );

        let Some(record) = db.get_bead_record(&bead.id)? else {
            anyhow::bail!("record should exist");
        };
        assert_eq!(record.bead.id, bead.id);
        assert_eq!(record.bead.title, bead.title);
        assert_eq!(record.bead.priority, bead.priority);
        assert_eq!(record.bead.created_at, bead.created_at);
        assert_eq!(record.bead.updated_at, bead.updated_at);
        assert_eq!(record.grove_status, GroveBeadStatus::Ready);
        assert!(record.declared_paths.is_empty());
        assert_eq!(record.metadata, json!({}));

        let listed = db.list_bead_records()?;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].bead.id, bead.id);

        let snapshot = db.dependency_snapshot(&bead.id)?;
        assert_eq!(snapshot.blocked_by, bead.blocked_by);
        assert_eq!(snapshot.blocks, bead.blocks);
        Ok(())
    }

    #[test]
    fn get_bead_record_defaults_runtime_fields_and_parses_sqlite_timestamps() -> Result<()> {
        let dir = tempdir()?;
        let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
            .map_err(|_| anyhow::anyhow!("temp path was not valid UTF-8"))?;
        let mut db = Database::open(&db_path)?;
        db.migrate()?;

        db.connection().execute(
            "INSERT INTO bead_cache(\
                bead_id, title, description, priority, issue_type, status, assignee,\
                labels_json, parent_ids_json, dependency_ids_json, dependent_ids_json, raw_json, synced_at\
            ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, NULL, ?6, '[]', '[]', '[]', ?7, CURRENT_TIMESTAMP)",
            rusqlite::params![
                "grove-fallback",
                "Fallback bead",
                1,
                "task",
                "open",
                "[\"area:test\"]",
                "{\"id\":\"grove-fallback\"}",
            ],
        )?;

        let Some(record) = db.get_bead_record(&BeadId::new("grove-fallback"))? else {
            anyhow::bail!("fallback bead should exist");
        };

        assert_eq!(record.grove_status, GroveBeadStatus::Idle);
        assert!(record.declared_paths.is_empty());
        assert_eq!(record.metadata, json!({}));
        assert!(record.circuit_breaker_state.is_none());
        assert_eq!(record.bead.created_at, record.synced_at);
        assert_eq!(record.bead.updated_at, record.bead.created_at);
        Ok(())
    }

    #[test]
    fn bead_record_round_trips_circuit_breaker_state() -> Result<()> {
        let dir = tempdir()?;
        let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
            .map_err(|_| anyhow::anyhow!("temp path was not valid UTF-8"))?;
        let mut db = Database::open(&db_path)?;
        db.migrate()?;

        insert_bead_cache_row(&db, "grove-breaker", "Breaker bead")?;
        let started_at: Timestamp = "2026-03-16T11:00:00Z".parse()?;
        let ended_at: Timestamp = "2026-03-16T11:10:00Z".parse()?;
        let run = db.record_run_started(RunStartInput {
            escalation_tier: grove_types::EscalationTier::FirstAttempt,
            run_id: RunId::new("run-breaker"),
            bead_id: BeadId::new("grove-breaker"),
            attempt_no: 1,
            started_at,
        })?;
        assert_eq!(run.status, RunStatus::Active);

        let breaker = CircuitBreakerState {
            state: grove_types::CircuitState::Open,
            no_progress_count: 3,
            same_error_count: 0,
            permission_denial_count: 0,
            last_error_fingerprint: Some("same-error".to_owned()),
            opened_at: Some(ended_at),
        };

        db.record_run_finished(
            &BeadId::new("grove-breaker"),
            RunFinishInput {
                run_id: RunId::new("run-breaker"),
                status: RunStatus::Failed,
                failure_class: Some(FailureClass::NoProgress),
                failure_detail: Some("stuck".to_owned()),
                ended_at,
                retry_after: None,
                circuit_breaker_state: Some(breaker.clone()),
            },
        )?;

        let bead = db.get_bead_record(&BeadId::new("grove-breaker"))?.unwrap();
        assert_eq!(bead.circuit_breaker_state, Some(breaker));
        Ok(())
    }

    #[test]
    fn reservation_acquire_reports_held_run_id_without_falling_back_to_request_run() -> Result<()> {
        let dir = tempdir()?;
        let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
            .map_err(|_| anyhow::anyhow!("temp path was not valid UTF-8"))?;
        let mut db = Database::open(&db_path)?;
        db.migrate()?;

        insert_bead_cache_row(&db, "grove-held", "Held bead")?;
        insert_bead_cache_row(&db, "grove-request", "Request bead")?;
        insert_run_row(&db, "run-request", "grove-request", "Active")?;

        let held_expires_at: Timestamp = "2099-03-16T12:30:00Z".parse()?;
        db.connection().execute(
            "INSERT INTO reservations(\
                bead_id, run_id, path_pattern, exclusive, reason, expires_at, released_at\
             ) VALUES (?1, NULL, ?2, ?3, ?4, ?5, NULL)",
            rusqlite::params![
                "grove-held",
                "crates/grove-db/src/lib.rs",
                1,
                "held work",
                held_expires_at.to_rfc3339(),
            ],
        )?;

        let request_expires_at: Timestamp = "2099-03-16T13:00:00Z".parse()?;
        let acquired_at: Timestamp = "2026-03-16T12:00:00Z".parse()?;
        let outcome = db.acquire_reservations(
            &BeadId::new("grove-request"),
            Some(&RunId::new("run-request")),
            &[crate::ReservationRequest {
                path_pattern: "crates/grove-db/src/lib.rs",
                mode: ReservationMode::Exclusive,
                reason: Some("request work"),
                expires_at: request_expires_at,
            }],
            &acquired_at,
        )?;

        assert!(outcome.acquired.is_empty());
        assert_eq!(outcome.conflicts.len(), 1);
        assert_eq!(outcome.conflicts[0].conflicting_bead.as_str(), "grove-held");
        assert_eq!(outcome.conflicts[0].conflicting_run_id, None);
        Ok(())
    }

    #[test]
    fn reservation_patterns_overlap_handles_file_and_common_glob_cases() {
        assert!(reservation_patterns_overlap(
            "crates/grove-db/src/lib.rs",
            "crates/grove-db/src/lib.rs"
        ));
        assert!(reservation_patterns_overlap(
            "crates/grove-db/src/lib.rs",
            "crates/grove-db/src/*"
        ));
        assert!(reservation_patterns_overlap(
            "crates/grove-db/src/*",
            "crates/grove-db/src/lib.rs"
        ));
        assert!(reservation_patterns_overlap(
            "crates/grove-db/**",
            "crates/grove-db/src/lib.rs"
        ));
        assert!(reservation_patterns_overlap(
            "crates/grove-db/src/*.rs",
            "crates/grove-db/src/lib.rs"
        ));
        assert!(reservation_patterns_overlap(
            "crates/grove-db/src/lib.rs",
            "crates/grove-db/src/*.rs"
        ));
        assert!(reservation_patterns_overlap(
            "crates/grove-db/src/**",
            "crates/grove-db/src/nested/lib.rs"
        ));
        assert!(!reservation_patterns_overlap(
            "crates/grove-db/src/*.rs",
            "crates/grove-db/src/nested/lib.rs"
        ));
        assert!(!reservation_patterns_overlap(
            "crates/grove-db/src/*.rs",
            "crates/grove-db/tests/*.rs"
        ));
        assert!(!reservation_patterns_overlap(
            "crates/grove-db/src/lib.rs",
            "crates/grove-kernel/src/lib.rs"
        ));
        assert!(!reservation_patterns_overlap("*.rs", "Cargo.toml"));
    }

    #[test]
    fn leader_lease_acquire_heartbeat_and_release_round_trip() -> Result<()> {
        let dir = tempdir()?;
        let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
            .map_err(|_| anyhow::anyhow!("temp path was not valid UTF-8"))?;
        let mut db = Database::open(&db_path)?;
        db.migrate()?;

        let acquired_at: Timestamp = "2026-03-16T12:00:00Z".parse()?;
        let lease = db
            .acquire_leader_lease(crate::LeaderLeaseAcquireInput {
                owner_label: "leader-a".to_owned(),
                run_id: None,
                acquired_at,
                expires_at: "2026-03-16T12:00:30Z".parse()?,
            })?
            .unwrap();
        assert_eq!(lease.owner_label, "leader-a");
        assert_eq!(lease.acquired_at, acquired_at);
        assert_eq!(lease.heartbeat_at, acquired_at);

        let contested = db.acquire_leader_lease(crate::LeaderLeaseAcquireInput {
            owner_label: "leader-b".to_owned(),
            run_id: None,
            acquired_at,
            expires_at: "2026-03-16T12:00:45Z".parse()?,
        })?;
        assert!(contested.is_none());

        let heartbeat_at: Timestamp = "2026-03-16T12:00:10Z".parse()?;
        let heartbeat = db
            .heartbeat_leader_lease("leader-a", &heartbeat_at, &"2026-03-16T12:00:40Z".parse()?)?
            .unwrap();
        assert_eq!(heartbeat.heartbeat_at, heartbeat_at);
        assert_eq!(
            heartbeat.expires_at,
            "2026-03-16T12:00:40Z".parse::<Timestamp>()?
        );

        let released = db
            .release_leader_lease("leader-a", &"2026-03-16T12:00:20Z".parse()?)?
            .unwrap();
        assert_eq!(released.owner_label, "leader-a");
        assert!(db
            .active_leader_lease(&"2026-03-16T12:00:20Z".parse()?)?
            .is_none());
        Ok(())
    }

    #[test]
    fn reconcile_interrupted_runs_marks_active_runs_failed() -> Result<()> {
        let dir = tempdir()?;
        let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
            .map_err(|_| anyhow::anyhow!("temp path was not valid UTF-8"))?;
        let mut db = Database::open(&db_path)?;
        db.migrate()?;
        insert_bead_cache_row(&db, "grove-interrupted", "Interrupted bead")?;
        insert_run_row(&db, "run-active", "grove-interrupted", "Active")?;

        let recovered = db.reconcile_interrupted_runs(&"2026-03-16T12:05:00Z".parse()?)?;
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].bead_id.as_str(), "grove-interrupted");
        assert_eq!(recovered[0].run.status, RunStatus::Failed);
        assert_eq!(
            recovered[0].run.failure_class,
            Some(FailureClass::Interrupted)
        );
        assert_eq!(
            recovered[0]
                .recovery_capsule
                .as_ref()
                .map(|capsule| capsule.outcome),
            Some(RecoveryCapsuleOutcome::Interrupted)
        );

        let bead = db
            .get_bead_record(&BeadId::new("grove-interrupted"))?
            .unwrap();
        assert_eq!(bead.grove_status, GroveBeadStatus::Failed);
        assert_eq!(bead.last_failure_class, Some(FailureClass::Interrupted));

        let capsule = db
            .latest_recovery_capsule_for_bead(&BeadId::new("grove-interrupted"))?
            .unwrap();
        assert_eq!(capsule.capsule.outcome, RecoveryCapsuleOutcome::Interrupted);
        assert!(capsule.capsule.summary.contains("persisted durable state"));
        Ok(())
    }

    #[test]
    fn recover_stale_reservations_releases_terminal_run_claims() -> Result<()> {
        let dir = tempdir()?;
        let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
            .map_err(|_| anyhow::anyhow!("temp path was not valid UTF-8"))?;
        let mut db = Database::open(&db_path)?;
        db.migrate()?;
        insert_bead_cache_row(&db, "grove-reservation", "Reservation bead")?;
        insert_run_row(&db, "run-terminal", "grove-reservation", "Failed")?;
        db.connection().execute(
            "INSERT INTO reservations(\
                bead_id, run_id, path_pattern, exclusive, reason, expires_at, released_at\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)",
            rusqlite::params![
                "grove-reservation",
                "run-terminal",
                "crates/grove-db/src/lib.rs",
                1,
                "recovery test",
                "2099-03-16T12:30:00Z",
            ],
        )?;

        let recovered = db.recover_stale_reservations(&"2026-03-16T12:05:00Z".parse()?)?;
        assert_eq!(recovered.len(), 1);
        assert_eq!(
            recovered[0].reservation.path_pattern,
            "crates/grove-db/src/lib.rs"
        );
        assert_eq!(
            recovered[0].reason,
            crate::RecoveryReason::RunNoLongerActive
        );
        assert!(db
            .list_active_reservations_at(&"2026-03-16T12:05:00Z".parse()?)?
            .is_empty());
        Ok(())
    }

    #[test]
    fn lifecycle_writes_persist_runs_sessions_checkpoints_and_runtime_state() -> Result<()> {
        let dir = tempdir()?;
        let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
            .map_err(|_| anyhow::anyhow!("temp path was not valid UTF-8"))?;
        let mut db = Database::open(&db_path)?;
        db.migrate()?;

        insert_bead_cache_row(&db, "grove-life", "Lifecycle bead")?;

        let started_at: Timestamp = "2026-03-16T11:00:00Z".parse()?;
        let run = db.record_run_started(RunStartInput {
            escalation_tier: grove_types::EscalationTier::FirstAttempt,
            run_id: RunId::new("run-life"),
            bead_id: BeadId::new("grove-life"),
            attempt_no: 1,
            started_at,
        })?;
        assert_eq!(run.status, RunStatus::Active);

        let session_started = ClaudeSessionRecord {
            id: SessionId::new("ses-life"),
            run_id: RunId::new("run-life"),
            external_session_id: Some("claude-life".to_owned()),
            ordinal_in_run: 1,
            status: SessionStatus::Running,
            started_at,
            ended_at: None,
            prompt_id: Some(PromptId::new("prompt-life")),
            prompt_manifest_path: Some(".grove/prompts/prompt-life.json".to_owned()),
            prompt_bytes: 120,
            estimated_input_tokens: 30,
            estimated_output_tokens: 0,
            exit_code: None,
            stop_reason: None,
            transcript_path: ".grove/transcripts/grove-life/ses-life.jsonl".to_owned(),
        };
        db.record_session_started(&BeadId::new("grove-life"), &session_started)?;

        let checkpoint = db.record_checkpoint_saved(SessionCheckpointInput {
            checkpoint_id: CheckpointId::new("chk-life"),
            bead_id: BeadId::new("grove-life"),
            run_id: RunId::new("run-life"),
            session_id: SessionId::new("ses-life"),
            payload: CheckpointPayload {
                progress: "halfway".to_owned(),
                next_step: "finish lifecycle".to_owned(),
                context: json!({"state":"checkpointed"}),
                open_questions: vec!["none".to_owned()],
                claimed_paths: vec!["crates/grove-db/src/lib.rs".to_owned()],
                confidence: Some(0.8),
            },
            saved_at: "2026-03-16T11:05:00Z".parse()?,
            resume_generation: 2,
        })?;
        assert_eq!(checkpoint.id.as_str(), "chk-life");
        assert_eq!(
            db.get_bead_record(&BeadId::new("grove-life"))?
                .unwrap()
                .declared_paths,
            vec!["crates/grove-db/src/lib.rs".to_owned()]
        );

        let session_finished = ClaudeSessionRecord {
            status: SessionStatus::Checkpointed,
            ended_at: Some("2026-03-16T11:06:00Z".parse()?),
            estimated_output_tokens: 45,
            exit_code: Some(0),
            stop_reason: Some(StopReason::Checkpoint),
            ..session_started.clone()
        };
        db.record_session_finished(&BeadId::new("grove-life"), &session_finished)?;

        let bead = db.get_bead_record(&BeadId::new("grove-life"))?.unwrap();
        assert_eq!(bead.grove_status, GroveBeadStatus::Checkpointed);
        assert_eq!(
            bead.declared_paths,
            vec!["crates/grove-db/src/lib.rs".to_owned()]
        );

        let finished_run = db.record_run_finished(
            &BeadId::new("grove-life"),
            RunFinishInput {
                run_id: RunId::new("run-life"),
                status: RunStatus::Checkpointed,
                failure_class: None,
                failure_detail: None,
                ended_at: "2026-03-16T11:07:00Z".parse()?,
                retry_after: None,
                circuit_breaker_state: None,
            },
        )?;
        assert_eq!(finished_run.status, RunStatus::Checkpointed);
        assert_eq!(finished_run.session_count, 1);
        assert_eq!(finished_run.checkpoint_count, 1);
        assert_eq!(
            finished_run
                .last_checkpoint_id
                .as_ref()
                .map(|id| id.as_str()),
            Some("chk-life")
        );

        let latest_session = db.latest_session_for_run(&RunId::new("run-life"))?.unwrap();
        assert_eq!(latest_session.status, SessionStatus::Checkpointed);
        assert_eq!(latest_session.stop_reason, Some(StopReason::Checkpoint));

        let latest_checkpoint = db
            .latest_checkpoint_for_bead(&BeadId::new("grove-life"))?
            .unwrap();
        assert_eq!(latest_checkpoint.next_step, "finish lifecycle");

        let bead = db.get_bead_record(&BeadId::new("grove-life"))?.unwrap();
        assert_eq!(bead.grove_status, GroveBeadStatus::Checkpointed);
        assert_eq!(
            bead.last_run_id.as_ref().map(|id| id.as_str()),
            Some("run-life")
        );
        assert_eq!(
            bead.declared_paths,
            vec!["crates/grove-db/src/lib.rs".to_owned()]
        );

        let events = db.list_event_logs_for_bead(&BeadId::new("grove-life"))?;
        let kinds = events.iter().map(|event| event.kind).collect::<Vec<_>>();
        assert!(kinds.contains(&EventKind::RunStarted));
        assert!(kinds.contains(&EventKind::RunCheckpointed));
        assert!(kinds.contains(&EventKind::SessionStarted));
        assert!(kinds.contains(&EventKind::SessionCheckpointed));
        Ok(())
    }

    #[test]
    fn lifecycle_writes_reject_cross_bead_or_cross_run_linkage() -> Result<()> {
        let dir = tempdir()?;
        let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
            .map_err(|_| anyhow::anyhow!("temp path was not valid UTF-8"))?;
        let mut db = Database::open(&db_path)?;
        db.migrate()?;

        insert_bead_cache_row(&db, "grove-a", "Lifecycle bead A")?;
        insert_bead_cache_row(&db, "grove-b", "Lifecycle bead B")?;

        let started_at: Timestamp = "2026-03-16T12:00:00Z".parse()?;
        db.record_run_started(RunStartInput {
            escalation_tier: grove_types::EscalationTier::FirstAttempt,
            run_id: RunId::new("run-a"),
            bead_id: BeadId::new("grove-a"),
            attempt_no: 1,
            started_at,
        })?;
        db.record_run_started(RunStartInput {
            escalation_tier: grove_types::EscalationTier::FirstAttempt,
            run_id: RunId::new("run-b"),
            bead_id: BeadId::new("grove-b"),
            attempt_no: 1,
            started_at,
        })?;
        db.record_run_started(RunStartInput {
            escalation_tier: grove_types::EscalationTier::FirstAttempt,
            run_id: RunId::new("run-a-2"),
            bead_id: BeadId::new("grove-a"),
            attempt_no: 2,
            started_at,
        })?;

        let session_a = ClaudeSessionRecord {
            id: SessionId::new("ses-a"),
            run_id: RunId::new("run-a"),
            external_session_id: None,
            ordinal_in_run: 1,
            status: SessionStatus::Running,
            started_at,
            ended_at: None,
            prompt_id: Some(PromptId::new("prompt-a")),
            prompt_manifest_path: Some(".grove/prompts/prompt-a.json".to_owned()),
            prompt_bytes: 10,
            estimated_input_tokens: 5,
            estimated_output_tokens: 0,
            exit_code: None,
            stop_reason: None,
            transcript_path: ".grove/transcripts/grove-a/ses-a.jsonl".to_owned(),
        };
        db.record_session_started(&BeadId::new("grove-a"), &session_a)?;

        let wrong_bead_err = db
            .record_session_finished(
                &BeadId::new("grove-b"),
                &ClaudeSessionRecord {
                    status: SessionStatus::Completed,
                    ended_at: Some("2026-03-16T12:05:00Z".parse()?),
                    estimated_output_tokens: 12,
                    exit_code: Some(0),
                    stop_reason: Some(StopReason::Exit),
                    ..session_a.clone()
                },
            )
            .expect_err("session finish should reject a mismatched bead");
        assert!(wrong_bead_err
            .to_string()
            .contains("belongs to bead grove-a, not grove-b"));

        let wrong_run_err = db.record_checkpoint_saved(SessionCheckpointInput {
            checkpoint_id: CheckpointId::new("chk-bad"),
            bead_id: BeadId::new("grove-a"),
            run_id: RunId::new("run-a"),
            session_id: SessionId::new("ses-a"),
            payload: CheckpointPayload {
                progress: "halfway".to_owned(),
                next_step: "verify linkage".to_owned(),
                context: json!({}),
                open_questions: Vec::new(),
                claimed_paths: vec!["crates/grove-db/src/lib.rs".to_owned()],
                confidence: None,
            },
            saved_at: "2026-03-16T12:06:00Z".parse()?,
            resume_generation: 1,
        })?;
        assert_eq!(wrong_run_err.run_id.as_str(), "run-a");

        let cross_run_session_err = db
            .record_checkpoint_saved(SessionCheckpointInput {
                checkpoint_id: CheckpointId::new("chk-cross-run"),
                bead_id: BeadId::new("grove-a"),
                run_id: RunId::new("run-a-2"),
                session_id: SessionId::new("ses-a"),
                payload: CheckpointPayload {
                    progress: "halfway".to_owned(),
                    next_step: "verify linkage".to_owned(),
                    context: json!({}),
                    open_questions: Vec::new(),
                    claimed_paths: vec!["crates/grove-db/src/lib.rs".to_owned()],
                    confidence: None,
                },
                saved_at: "2026-03-16T12:07:00Z".parse()?,
                resume_generation: 2,
            })
            .expect_err("checkpoint save should reject a mismatched session/run pair");
        assert!(cross_run_session_err
            .to_string()
            .contains("session ses-a belongs to run run-a, not run-a-2"));
        Ok(())
    }

    #[test]
    fn query_helpers_read_runs_sessions_checkpoints_handoffs_and_events() -> Result<()> {
        let dir = tempdir()?;
        let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
            .map_err(|_| anyhow::anyhow!("temp path was not valid UTF-8"))?;
        let mut db = Database::open(&db_path)?;
        db.migrate()?;

        db.connection().execute(
            "INSERT INTO bead_cache(\
                bead_id, title, description, priority, issue_type, status, assignee,\
                labels_json, parent_ids_json, dependency_ids_json, dependent_ids_json, raw_json, synced_at\
            ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, NULL, '[]', '[]', '[]', '[]', ?6, ?7)",
            rusqlite::params![
                "grove-query",
                "Query bead",
                0,
                "task",
                "open",
                "{}",
                "2026-03-16T10:00:00Z",
            ],
        )?;

        db.connection().execute(
            "INSERT INTO task_runs(\
                id, bead_id, attempt_no, status, failure_class, failure_detail, started_at, ended_at, session_count, checkpoint_count, last_checkpoint_id\
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                "run-query",
                "grove-query",
                2,
                "Checkpointed",
                "RateLimit",
                "wait before retry",
                "2026-03-16T11:00:00Z",
                "2026-03-16T11:10:00Z",
                1,
                1,
                "chk-query",
            ],
        )?;

        db.connection().execute(
            "INSERT INTO claude_sessions(\
                id, run_id, external_session_id, ordinal_in_run, status, started_at, ended_at, prompt_id, prompt_manifest_path, prompt_bytes, estimated_input_tokens, estimated_output_tokens, exit_code, stop_reason, transcript_path\
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            rusqlite::params![
                "ses-query",
                "run-query",
                "claude-123",
                1,
                "Checkpointed",
                "2026-03-16T11:00:00Z",
                "2026-03-16T11:05:00Z",
                "prompt-query",
                ".grove/prompts/prompt-query.json",
                120,
                30,
                45,
                0,
                "Checkpoint",
                ".grove/transcripts/grove-query/ses-query.jsonl",
            ],
        )?;

        db.connection().execute(
            "INSERT INTO checkpoints(\
                id, bead_id, run_id, session_id, progress, next_step, payload_json, saved_at, resume_generation\
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                "chk-query",
                "grove-query",
                "run-query",
                "ses-query",
                "halfway there",
                "finish the query layer",
                "{\"claimed_paths\":[\"crates/grove-db/src/lib.rs\"]}",
                "2026-03-16T11:06:00Z",
                3,
            ],
        )?;

        db.connection().execute(
            "INSERT INTO handoffs(\
                bead_id, run_id, summary, artifacts_json, lessons_json, decisions_json, warnings_json, completed_at\
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                "grove-query",
                "run-query",
                "finished query helpers",
                "[\"artifact-1\"]",
                "[\"lesson-1\"]",
                "[\"decision-1\"]",
                "[\"warning-1\"]",
                "2026-03-16T11:20:00Z",
            ],
        )?;

        db.connection().execute(
            "INSERT INTO event_log(kind, bead_id, run_id, session_id, payload_json, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                "BrMirrorFailed",
                "grove-query",
                "run-query",
                "ses-query",
                "{\"error\":\"network hiccup\"}",
                "2026-03-16T11:21:00Z",
            ],
        )?;

        db.connection().execute(
            "INSERT INTO reservations(\
                bead_id, run_id, path_pattern, exclusive, reason, expires_at, released_at\
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)",
            rusqlite::params![
                "grove-query",
                "run-query",
                "crates/grove-db/src/lib.rs",
                1,
                "query helper work",
                "2099-03-16T12:30:00Z",
            ],
        )?;

        let runs = db.list_task_runs_for_bead(&BeadId::new("grove-query"))?;
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id.as_str(), "run-query");
        assert_eq!(format!("{:?}", runs[0].status), "Checkpointed");
        assert_eq!(format!("{:?}", runs[0].failure_class), "Some(RateLimit)");
        assert_eq!(
            runs[0].last_checkpoint_id.as_ref().map(|id| id.as_str()),
            Some("chk-query")
        );

        let session = db
            .latest_session_for_run(&RunId::new("run-query"))?
            .ok_or_else(|| anyhow::anyhow!("expected latest session"))?;
        assert_eq!(session.id.as_str(), "ses-query");
        assert_eq!(format!("{:?}", session.status), "Checkpointed");
        assert_eq!(format!("{:?}", session.stop_reason), "Some(Checkpoint)");
        assert_eq!(
            session.prompt_id.as_ref().map(|id| id.as_str()),
            Some("prompt-query")
        );
        assert_eq!(
            session.prompt_manifest_path.as_deref(),
            Some(".grove/prompts/prompt-query.json")
        );

        let checkpoint = db
            .latest_checkpoint_for_bead(&BeadId::new("grove-query"))?
            .ok_or_else(|| anyhow::anyhow!("expected latest checkpoint"))?;
        assert_eq!(checkpoint.id.as_str(), "chk-query");
        assert_eq!(checkpoint.resume_generation, 3);
        assert_eq!(checkpoint.progress, "halfway there");

        let handoff = db
            .handoff_for_bead(&BeadId::new("grove-query"))?
            .ok_or_else(|| anyhow::anyhow!("expected handoff"))?;
        assert_eq!(handoff.summary, "finished query helpers");
        assert_eq!(handoff.artifacts, vec!["artifact-1"]);

        let events = db.list_event_logs_for_bead(&BeadId::new("grove-query"))?;
        assert_eq!(events.len(), 1);
        assert_eq!(format!("{:?}", events[0].kind), "BrMirrorFailed");
        assert_eq!(
            events[0].run_id.as_ref().map(|id| id.as_str()),
            Some("run-query")
        );
        assert!(events[0].payload.to_string().contains("network hiccup"));

        let reservations = db.list_active_reservations()?;
        assert_eq!(reservations.len(), 1);
        assert_eq!(reservations[0].bead_id.as_str(), "grove-query");
        assert_eq!(
            reservations[0].run_id.as_ref().map(|id| id.as_str()),
            Some("run-query")
        );
        assert_eq!(reservations[0].path_pattern, "crates/grove-db/src/lib.rs");
        assert_eq!(reservations[0].mode, ReservationMode::Exclusive);
        Ok(())
    }

    #[test]
    fn mark_mirror_in_progress_tracks_attempt_timestamp_and_count() -> Result<()> {
        let dir = tempdir()?;
        let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
            .map_err(|_| anyhow::anyhow!("temp path was not valid UTF-8"))?;
        let mut db = Database::open(&db_path)?;
        db.migrate()?;
        insert_bead_cache_row(&db, "grove-mirror-progress", "Mirror progress bead")?;
        insert_run_row(
            &db,
            "run-mirror-progress",
            "grove-mirror-progress",
            "Succeeded",
        )?;

        let handoff = HandoffRecord {
            bead_id: BeadId::new("grove-mirror-progress"),
            run_id: RunId::new("run-mirror-progress"),
            summary: "done".into(),
            artifacts: Vec::new(),
            lessons: Vec::new(),
            decisions: Vec::new(),
            warnings: Vec::new(),
            completed_at: "2026-03-20T06:00:00Z".parse()?,
        };
        let operation = db.enqueue_mirror_outbox(
            &BeadId::new("grove-mirror-progress"),
            &RunId::new("run-mirror-progress"),
            &handoff,
            true,
        )?;

        db.mark_mirror_in_progress(&operation.id)?;

        let pending = db.connection().query_row(
            "SELECT mirror_status, attempt_count, last_attempt_at FROM mirror_outbox WHERE id = ?1",
            [&operation.id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i32>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )?;
        assert_eq!(pending.0, "in_progress");
        assert_eq!(pending.1, 1);
        assert!(pending.2.is_some());
        Ok(())
    }

    #[test]
    fn record_mirror_success_clears_retry_metadata_and_links_bead_event() -> Result<()> {
        let dir = tempdir()?;
        let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
            .map_err(|_| anyhow::anyhow!("temp path was not valid UTF-8"))?;
        let mut db = Database::open(&db_path)?;
        db.migrate()?;
        insert_bead_cache_row(&db, "grove-mirror-success", "Mirror success bead")?;
        insert_run_row(
            &db,
            "run-mirror-success",
            "grove-mirror-success",
            "Succeeded",
        )?;

        let handoff = HandoffRecord {
            bead_id: BeadId::new("grove-mirror-success"),
            run_id: RunId::new("run-mirror-success"),
            summary: "done".into(),
            artifacts: Vec::new(),
            lessons: Vec::new(),
            decisions: Vec::new(),
            warnings: Vec::new(),
            completed_at: "2026-03-20T06:05:00Z".parse()?,
        };
        let operation = db.enqueue_mirror_outbox(
            &BeadId::new("grove-mirror-success"),
            &RunId::new("run-mirror-success"),
            &handoff,
            true,
        )?;
        db.mark_mirror_in_progress(&operation.id)?;
        let retry_after: chrono::DateTime<Utc> = "2026-03-20T06:10:00Z".parse()?;
        db.record_mirror_failure(
            &operation.id,
            &RunId::new("run-mirror-success"),
            "temporary error",
            Some(&retry_after),
        )?;
        db.record_mirror_success(&operation.id, &RunId::new("run-mirror-success"))?;

        let row = db
            .connection()
            .query_row(
                "SELECT mirror_status, attempt_count, last_attempt_at, next_retry_after, last_error FROM mirror_outbox WHERE id = ?1",
                [&operation.id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i32>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                },
            )?;
        assert_eq!(row.0, "succeeded");
        assert_eq!(row.1, 1);
        assert!(row.2.is_some());
        assert!(row.3.is_none());
        assert!(row.4.is_none());

        let events = db.list_event_logs_for_bead(&BeadId::new("grove-mirror-success"))?;
        assert!(events
            .iter()
            .any(|event| event.kind == EventKind::BrMirrorSucceeded));
        Ok(())
    }

    #[test]
    fn record_mirror_failure_links_event_to_bead() -> Result<()> {
        let dir = tempdir()?;
        let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
            .map_err(|_| anyhow::anyhow!("temp path was not valid UTF-8"))?;
        let mut db = Database::open(&db_path)?;
        db.migrate()?;
        insert_bead_cache_row(&db, "grove-mirror-failure", "Mirror failure bead")?;
        insert_run_row(
            &db,
            "run-mirror-failure",
            "grove-mirror-failure",
            "Succeeded",
        )?;

        let handoff = HandoffRecord {
            bead_id: BeadId::new("grove-mirror-failure"),
            run_id: RunId::new("run-mirror-failure"),
            summary: "done".into(),
            artifacts: Vec::new(),
            lessons: Vec::new(),
            decisions: Vec::new(),
            warnings: Vec::new(),
            completed_at: "2026-03-20T06:15:00Z".parse()?,
        };
        let operation = db.enqueue_mirror_outbox(
            &BeadId::new("grove-mirror-failure"),
            &RunId::new("run-mirror-failure"),
            &handoff,
            true,
        )?;
        db.mark_mirror_in_progress(&operation.id)?;
        db.record_mirror_failure(
            &operation.id,
            &RunId::new("run-mirror-failure"),
            "network hiccup",
            None,
        )?;

        let events = db.list_event_logs_for_bead(&BeadId::new("grove-mirror-failure"))?;
        assert!(events
            .iter()
            .any(|event| event.kind == EventKind::BrMirrorFailed));
        Ok(())
    }

    struct FakeBrClient {
        ready: Vec<BrIssueSummary>,
        list_open: Vec<BrIssueSummary>,
        dep_snapshots: BTreeMap<String, BrDependencySnapshot>,
    }

    impl BrClient for FakeBrClient {
        fn ready(&self) -> Result<Vec<BrIssueSummary>, BrError> {
            Ok(self.ready.clone())
        }

        fn list_open(&self) -> Result<Vec<BrIssueSummary>, BrError> {
            Ok(self.list_open.clone())
        }

        fn show(&self, id: &BeadId) -> Result<BrIssueDetail, BrError> {
            Err(BrError::BeadNotFound { id: id.clone() })
        }

        fn dep_list(&self, id: &BeadId) -> Result<BrDependencySnapshot, BrError> {
            self.dep_snapshots
                .get(id.as_str())
                .cloned()
                .ok_or_else(|| BrError::BeadNotFound { id: id.clone() })
        }

        fn capability(&self) -> Result<BrCapability, BrError> {
            Ok(BrCapability {
                available: true,
                version_line: Some("br 0.1.12".into()),
                version: Some(BrVersion {
                    raw: "br 0.1.12".into(),
                    major: Some(0),
                    minor: Some(1),
                    patch: Some(12),
                }),
                beads_dir_exists: true,
            })
        }

        fn close_bead(&self, _id: &BeadId, _reason: Option<&str>) -> Result<(), BrError> {
            // Fake implementation - always succeeds
            Ok(())
        }

        fn add_comment(&self, _id: &BeadId, _text: &str) -> Result<(), BrError> {
            // Fake implementation - always succeeds
            Ok(())
        }

        fn mirror_handoff(
            &self,
            _id: &BeadId,
            _handoff: &HandoffRecord,
            _close_bead: bool,
        ) -> Result<(), BrError> {
            // Fake implementation - always succeeds
            Ok(())
        }
    }

    fn insert_bead_cache_row(db: &Database, bead_id: &str, title: &str) -> Result<()> {
        db.connection().execute(
            "INSERT INTO bead_cache(\
                bead_id, title, description, priority, issue_type, status, assignee,\
                labels_json, parent_ids_json, dependency_ids_json, dependent_ids_json, raw_json, synced_at\
             ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, NULL, '[]', '[]', '[]', '[]', ?6, ?7)",
            rusqlite::params![
                bead_id,
                title,
                1,
                "task",
                "open",
                json!({"id": bead_id, "title": title}).to_string(),
                "2026-03-16T10:00:00Z",
            ],
        )?;
        Ok(())
    }

    fn insert_run_row(db: &Database, run_id: &str, bead_id: &str, status: &str) -> Result<()> {
        db.connection().execute(
            "INSERT INTO task_runs(\
                id, bead_id, attempt_no, status, failure_class, failure_detail, started_at, ended_at, session_count, checkpoint_count, last_checkpoint_id, activity, last_activity_at, escalation_tier\
             ) VALUES (?1, ?2, ?3, ?4, NULL, NULL, ?5, NULL, 0, 0, NULL, ?6, ?7, ?8)",
            rusqlite::params![
                run_id,
                bead_id,
                1,
                status,
                "2026-03-16T11:00:00Z",
                "Active",
                "2026-03-16T11:00:00Z",
                "FirstAttempt"
            ],
        )?;
        Ok(())
    }

    #[test]
    fn run_activity_and_escalation_round_trip() -> Result<()> {
        let dir = tempdir()?;
        let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
            .map_err(|_| anyhow::anyhow!("temp path was not valid UTF-8"))?;
        let mut db = Database::open(&db_path)?;
        db.migrate()?;
        insert_bead_cache_row(&db, "grove-activity", "Activity bead")?;

        let started = db.record_run_started(RunStartInput {
            run_id: RunId::new("run-activity"),
            bead_id: BeadId::new("grove-activity"),
            attempt_no: 1,
            started_at: "2026-03-16T11:00:00Z".parse()?,
            escalation_tier: grove_types::EscalationTier::FirstAttempt,
        })?;
        assert_eq!(started.activity, Some(grove_types::AgentActivity::Active));
        assert_eq!(
            started.escalation_tier,
            grove_types::EscalationTier::FirstAttempt
        );

        let updated_at: Timestamp = "2026-03-16T11:05:00Z".parse()?;
        db.update_run_activity(
            &BeadId::new("grove-activity"),
            &RunId::new("run-activity"),
            grove_types::AgentActivity::Idle,
            &updated_at,
        )?;
        db.update_run_escalation_tier(
            &BeadId::new("grove-activity"),
            &RunId::new("run-activity"),
            grove_types::EscalationTier::ThirdAttempt,
            &updated_at,
        )?;

        let runs = db.list_task_runs_for_bead(&BeadId::new("grove-activity"))?;
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].activity, Some(grove_types::AgentActivity::Idle));
        assert_eq!(runs[0].last_activity_at, Some(updated_at));
        assert_eq!(
            runs[0].escalation_tier,
            grove_types::EscalationTier::ThirdAttempt
        );
        Ok(())
    }

    #[test]
    fn run_metrics_aggregation_returns_none_for_empty_run() -> Result<()> {
        let dir = tempdir()?;
        let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
            .map_err(|_| anyhow::anyhow!("temp path was not valid UTF-8"))?;
        let mut db = Database::open(&db_path)?;
        db.migrate()?;
        insert_bead_cache_row(&db, "grove-metrics", "Metrics bead")?;

        let metrics = db.aggregate_run_metrics(&RunId::new("run-nonexistent"))?;
        assert!(metrics.is_none());

        let report = db.generate_run_report(&RunId::new("run-nonexistent"))?;
        assert!(report.is_none());

        Ok(())
    }

    #[test]
    fn run_metrics_aggregation_computes_correct_values() -> Result<()> {
        let dir = tempdir()?;
        let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
            .map_err(|_| anyhow::anyhow!("temp path was not valid UTF-8"))?;
        let mut db = Database::open(&db_path)?;
        db.migrate()?;
        insert_bead_cache_row(&db, "grove-metrics-test", "Metrics test bead")?;

        let started = db.record_run_started(RunStartInput {
            run_id: RunId::new("run-metrics-test"),
            bead_id: BeadId::new("grove-metrics-test"),
            attempt_no: 1,
            started_at: "2026-03-16T11:00:00Z".parse()?,
            escalation_tier: grove_types::EscalationTier::FirstAttempt,
        })?;
        assert_eq!(started.activity, Some(grove_types::AgentActivity::Active));

        let events = db.list_events_for_run(&RunId::new("run-metrics-test"))?;
        assert!(!events.is_empty());

        let metrics = db.aggregate_run_metrics(&RunId::new("run-metrics-test"))?;
        assert!(metrics.is_some());
        let metrics = metrics.unwrap();
        assert_eq!(metrics.run_id.as_str(), "run-metrics-test");
        assert_eq!(metrics.checkpoints_taken, 0);
        assert_eq!(metrics.retries_attempted, 0);

        let report = db.generate_run_report(&RunId::new("run-metrics-test"))?;
        assert!(report.is_some());
        let report = report.unwrap();
        assert_eq!(report.run_id.as_str(), "run-metrics-test");
        assert_eq!(report.bead_id.as_str(), "grove-metrics-test");
        assert_eq!(report.event_count, events.len() as u32);
        assert!(report.first_event_at.is_some());
        assert!(report.last_event_at.is_some());

        Ok(())
    }

    #[test]
    fn run_report_includes_failure_info() -> Result<()> {
        let dir = tempdir()?;
        let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
            .map_err(|_| anyhow::anyhow!("temp path was not valid UTF-8"))?;
        let mut db = Database::open(&db_path)?;
        db.migrate()?;
        insert_bead_cache_row(&db, "grove-failure-test", "Failure test bead")?;

        let started = db.record_run_started(RunStartInput {
            run_id: RunId::new("run-failure-test"),
            bead_id: BeadId::new("grove-failure-test"),
            attempt_no: 1,
            started_at: "2026-03-16T11:00:00Z".parse()?,
            escalation_tier: grove_types::EscalationTier::FirstAttempt,
        })?;

        let finished = db.record_run_finished(
            &BeadId::new("grove-failure-test"),
            RunFinishInput {
                run_id: RunId::new("run-failure-test"),
                status: grove_types::RunStatus::Failed,
                failure_class: Some(grove_types::FailureClass::Timeout),
                failure_detail: Some("Test timeout".to_owned()),
                ended_at: "2026-03-16T11:10:00Z".parse()?,
                retry_after: None,
                circuit_breaker_state: None,
            },
        )?;

        assert_eq!(finished.status, grove_types::RunStatus::Failed);

        let report = db.generate_run_report(&RunId::new("run-failure-test"))?;
        assert!(report.is_some());
        let report = report.unwrap();
        assert_eq!(report.status, grove_types::RunStatus::Failed);
        assert_eq!(
            report.failure_class,
            Some(grove_types::FailureClass::Timeout)
        );
        assert!(report.metrics.total_duration_secs > 0);

        Ok(())
    }

    fn sample_issue(
        id: &str,
        title: &str,
        blocked_by: Vec<BeadId>,
        blocks: Vec<BeadId>,
    ) -> Result<BrIssueSummary> {
        let created_at: Timestamp = "2026-03-16T10:00:00Z".parse()?;
        let updated_at: Timestamp = "2026-03-16T11:00:00Z".parse()?;

        Ok(BrIssueSummary {
            id: BeadId::new(id),
            title: title.into(),
            description: Some(format!("description for {title}")),
            priority: BeadPriority::P1,
            issue_type: "task".into(),
            status: "open".into(),
            assignee: None,
            labels: vec!["area:test".into()],
            created_at,
            updated_at,
            blocked_by,
            blocks,
            raw_json: json!({
                "id": id,
                "title": title,
                "created_at": created_at.to_rfc3339(),
                "updated_at": updated_at.to_rfc3339(),
            }),
        })
    }
}
