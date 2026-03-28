
#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::reservation_patterns_overlap;
use anyhow::Result;
use camino::Utf8PathBuf;
use chrono::Utc;
use grove_br::{
    BeadCacheStore, BrCapability, BrClient, BrDependencySnapshot, BrError, BrIssueDetail,
    BrIssueSummary, BrVersion, sync_bead_cache,
};
use grove_types::{
    BeadId, BeadPriority, CheckpointId, CheckpointPayload, CircuitBreakerState,
    ClaudeSessionRecord, EventKind, FailureClass, HandoffRecord, PromptId, RecoveryCapsuleOutcome,
    ReservationMode, RunId, RunStatus, RuntimeProvider, SessionId, SessionStatus, StopReason,
    Timestamp,
};
use rusqlite::OptionalExtension;
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
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
    assert_eq!(migrations.len(), 12);
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
    assert_eq!(
        migrations[11],
        MigrationState {
            version: 12,
            name: "0012_session_provider.sql".into(),
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

    let count: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM bead_cache", [], |row| row.get(0))?;
    assert_eq!(count, 1);
    Ok(())
}

#[test]
fn reset_bead_for_retry_with_action_records_custom_recovery_event() -> Result<()> {
    let dir = tempdir()?;
    let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
        .map_err(|_| anyhow::anyhow!("temp path was not valid UTF-8"))?;
    let mut db = Database::open(&db_path)?;
    db.migrate()?;

    let bead = sample_issue("grove-reset", "reset bead", vec![], vec![])?;
    db.upsert_bead_cache(&bead)?;
    let now = Utc::now();
    db.set_grove_status(&bead.id, GroveBeadStatus::Failed)?;
    db.connection().execute(
        "UPDATE bead_runtime SET retry_after = ?2, circuit_breaker_json = ?3 WHERE bead_id = ?1",
        rusqlite::params![
            bead.id.as_str(),
            now.to_rfc3339(),
            serde_json::json!({"state":"open"}).to_string()
        ],
    )?;

    db.reset_bead_for_retry_with_action(
        &bead.id,
        &now,
        serde_json::json!({
            "action": "autonomous_retry_reset",
            "trigger": "dispatch_blocked",
            "previous_failure_class": "claude_crashed"
        }),
    )?;

    let runtime = db
        .get_bead_record(&bead.id)?
        .expect("bead runtime should exist after reset");
    assert_eq!(runtime.grove_status, GroveBeadStatus::Ready);
    assert_eq!(runtime.retry_after, None);
    assert_eq!(runtime.circuit_breaker_state, None);

    let payload_json: String = db.connection().query_row(
        "SELECT payload_json FROM event_log WHERE kind = ?1 ORDER BY id DESC LIMIT 1",
        [super::encode_event_kind(EventKind::RecoveryActionTaken)],
        |row| row.get(0),
    )?;
    let payload: serde_json::Value = serde_json::from_str(&payload_json)?;
    assert_eq!(payload["action"], "autonomous_retry_reset");
    assert_eq!(payload["trigger"], "dispatch_blocked");
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
        dep_snapshots: BTreeMap::from([(bead.id.as_str().to_owned(), bead.dependency_snapshot())]),
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
    assert!(
        db.active_leader_lease(&"2026-03-16T12:00:20Z".parse()?)?
            .is_none()
    );
    Ok(())
}

#[test]
fn reconcile_interrupted_runs_marks_active_runs_retryable() -> Result<()> {
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
    assert_eq!(recovered[0].run.status, RunStatus::WaitingToRetry);
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
    assert_eq!(bead.grove_status, GroveBeadStatus::WaitingToRetry);
    assert_eq!(bead.last_failure_class, Some(FailureClass::Interrupted));

    let capsule = db
        .latest_recovery_capsule_for_bead(&BeadId::new("grove-interrupted"))?
        .unwrap();
    assert_eq!(capsule.capsule.outcome, RecoveryCapsuleOutcome::Interrupted);
    assert!(capsule.capsule.summary.contains("persisted durable state"));
    Ok(())
}

#[test]
fn reconcile_stale_unknown_plan_approval_runs_marks_them_retryable() -> Result<()> {
    let dir = tempdir()?;
    let workspace_dir = dir.path().join("workspace");
    fs::create_dir_all(workspace_dir.join(".grove/transcripts/grove-stale"))?;
    let transcript_path = workspace_dir.join(".grove/transcripts/grove-stale/ses-stale.jsonl");
    fs::write(
        &transcript_path,
        concat!(
            "{\"ts\":\"2026-03-16T11:00:00Z\",\"kind\":\"session_started\",\"session_id\":\"ses-stale\"}\n",
            "{\"ts\":\"2026-03-16T11:01:00Z\",\"kind\":\"stdout\",\"line\":\"Implemented the plan file and requested approval.\"}\n",
            "{\"ts\":\"2026-03-16T11:02:00Z\",\"kind\":\"session_ended\",\"exit_code\":0}\n"
        ),
    )?;
    let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
        .map_err(|_| anyhow::anyhow!("temp path was not valid UTF-8"))?;
    let mut db = Database::open(&db_path)?;
    db.migrate()?;
    insert_bead_cache_row(&db, "grove-stale", "Stale plan bead")?;
    db.connection().execute(
            "INSERT INTO task_runs(\
                id, bead_id, attempt_no, status, failure_class, failure_detail, started_at, ended_at, session_count, checkpoint_count, last_checkpoint_id, activity, last_activity_at, escalation_tier\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, NULL, ?11, ?12, ?13)",
            rusqlite::params![
                "run-stale",
                "grove-stale",
                1,
                "Failed",
                "Unknown",
                "session ended with Unknown",
                "2026-03-16T11:00:00Z",
                "2026-03-16T11:02:00Z",
                1,
                0,
                "Exited",
                "2026-03-16T11:02:00Z",
                "FirstAttempt",
            ],
        )?;
    db.connection().execute(
            "INSERT INTO bead_runtime(\
                bead_id, grove_status, declared_paths_json, metadata_json, last_run_id, retry_after,\
                last_failure_class, last_failure_detail, runtime_updated_at\
             ) VALUES (?1, ?2, '[]', '{}', ?3, NULL, ?4, ?5, ?6)",
            rusqlite::params![
                "grove-stale",
                "Failed",
                "run-stale",
                "Unknown",
                "session ended with Unknown",
                "2026-03-16T11:02:00Z",
            ],
        )?;
    db.connection().execute(
            "INSERT INTO claude_sessions(\
                id, run_id, external_session_id, ordinal_in_run, status, started_at, ended_at, prompt_id, prompt_manifest_path, prompt_bytes, estimated_input_tokens, estimated_output_tokens, exit_code, stop_reason, transcript_path\
             ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, NULL, NULL, 0, 0, 0, ?7, ?8, ?9)",
            rusqlite::params![
                "ses-stale",
                "run-stale",
                1,
                "UnknownFailure",
                "2026-03-16T11:00:00Z",
                "2026-03-16T11:02:00Z",
                0,
                "Unknown",
                transcript_path.to_string_lossy(),
            ],
        )?;

    let recovered = db.reconcile_interrupted_runs(&"2026-03-16T12:05:00Z".parse()?)?;
    let recovered = recovered
        .into_iter()
        .find(|entry| entry.run.id == RunId::new("run-stale"))
        .expect("stale run should be recovered");
    assert_eq!(recovered.run.status, RunStatus::WaitingToRetry);
    assert_eq!(recovered.run.failure_class, Some(FailureClass::NoProgress));

    let bead = db.get_bead_record(&BeadId::new("grove-stale"))?.unwrap();
    assert_eq!(bead.grove_status, GroveBeadStatus::WaitingToRetry);
    assert_eq!(bead.last_failure_class, Some(FailureClass::NoProgress));
    Ok(())
}

#[test]
fn reconcile_stale_unknown_empty_transcript_runs_marks_them_retryable() -> Result<()> {
    let dir = tempdir()?;
    let workspace_dir = dir.path().join("workspace");
    fs::create_dir_all(workspace_dir.join(".grove/transcripts/grove-empty"))?;
    let transcript_path = workspace_dir.join(".grove/transcripts/grove-empty/ses-empty.jsonl");
    fs::write(
        &transcript_path,
        concat!(
            "{\"ts\":\"2026-03-16T11:00:00Z\",\"kind\":\"session_started\",\"session_id\":\"ses-empty\"}\n",
            "{\"ts\":\"2026-03-16T11:02:00Z\",\"kind\":\"session_ended\"}\n"
        ),
    )?;
    let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
        .map_err(|_| anyhow::anyhow!("temp path was not valid UTF-8"))?;
    let mut db = Database::open(&db_path)?;
    db.migrate()?;
    insert_bead_cache_row(&db, "grove-empty", "Empty transcript bead")?;
    db.connection().execute(
            "INSERT INTO task_runs(\
                id, bead_id, attempt_no, status, failure_class, failure_detail, started_at, ended_at, session_count, checkpoint_count, last_checkpoint_id, activity, last_activity_at, escalation_tier\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, NULL, ?11, ?12, ?13)",
            rusqlite::params![
                "run-empty",
                "grove-empty",
                1,
                "Failed",
                "Unknown",
                "session ended with Unknown",
                "2026-03-16T11:00:00Z",
                "2026-03-16T11:02:00Z",
                1,
                0,
                "Exited",
                "2026-03-16T11:02:00Z",
                "FirstAttempt",
            ],
        )?;
    db.connection().execute(
            "INSERT INTO bead_runtime(\
                bead_id, grove_status, declared_paths_json, metadata_json, last_run_id, retry_after,\
                last_failure_class, last_failure_detail, runtime_updated_at\
             ) VALUES (?1, ?2, '[]', '{}', ?3, NULL, ?4, ?5, ?6)",
            rusqlite::params![
                "grove-empty",
                "Failed",
                "run-empty",
                "Unknown",
                "session ended with Unknown",
                "2026-03-16T11:02:00Z",
            ],
        )?;
    db.connection().execute(
            "INSERT INTO claude_sessions(\
                id, run_id, external_session_id, ordinal_in_run, status, started_at, ended_at, prompt_id, prompt_manifest_path, prompt_bytes, estimated_input_tokens, estimated_output_tokens, exit_code, stop_reason, transcript_path\
             ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, NULL, NULL, 0, 0, 0, NULL, ?7, ?8)",
            rusqlite::params![
                "ses-empty",
                "run-empty",
                1,
                "UnknownFailure",
                "2026-03-16T11:00:00Z",
                "2026-03-16T11:02:00Z",
                "Unknown",
                transcript_path.to_string_lossy(),
            ],
        )?;

    let recovered = db.reconcile_interrupted_runs(&"2026-03-16T12:05:00Z".parse()?)?;
    let recovered = recovered
        .into_iter()
        .find(|entry| entry.run.id == RunId::new("run-empty"))
        .expect("empty transcript run should be recovered");
    assert_eq!(recovered.run.status, RunStatus::WaitingToRetry);
    assert_eq!(recovered.run.failure_class, Some(FailureClass::NoProgress));

    let bead = db.get_bead_record(&BeadId::new("grove-empty"))?.unwrap();
    assert_eq!(bead.grove_status, GroveBeadStatus::WaitingToRetry);
    assert_eq!(bead.last_failure_class, Some(FailureClass::NoProgress));
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
    assert!(
        db.list_active_reservations_at(&"2026-03-16T12:05:00Z".parse()?)?
            .is_empty()
    );
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
        provider: RuntimeProvider::Claude,
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
        provider: RuntimeProvider::Claude,
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
    assert!(
        wrong_bead_err
            .to_string()
            .contains("belongs to bead grove-a, not grove-b")
    );

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
    assert!(
        cross_run_session_err
            .to_string()
            .contains("session ses-a belongs to run run-a, not run-a-2")
    );
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
    assert!(
        events
            .iter()
            .any(|event| event.kind == EventKind::BrMirrorSucceeded)
    );
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
    assert!(
        events
            .iter()
            .any(|event| event.kind == EventKind::BrMirrorFailed)
    );
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

    let _started = db.record_run_started(RunStartInput {
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
