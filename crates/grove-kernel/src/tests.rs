
#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use grove_types::{
    BeadId, BeadPriority, BeadRef, CircuitBreakerState, CircuitState, RunId, RunStatus,
};
use std::{error::Error, io};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

fn require_some<T>(value: Option<T>, message: &str) -> TestResult<T> {
    value.ok_or_else(|| io::Error::other(message).into())
}

#[test]
fn ready_bead_without_local_blockers_is_dispatchable() -> TestResult {
    let bead = sample_bead(GroveBeadStatus::Ready, "task", &[], None, None)?;
    let context = sample_context(true, CircuitState::Closed, Vec::new())?;

    let eligibility = evaluate_dispatch_eligibility(&bead, &context);

    assert!(eligibility.ready_in_br);
    assert!(eligibility.dispatchable_in_grove);
    assert!(!eligibility.has_local_suppressions());
    Ok(())
}

#[test]
fn not_ready_in_br_never_becomes_dispatchable() -> TestResult {
    let bead = sample_bead(GroveBeadStatus::Ready, "task", &[], None, None)?;
    let context = sample_context(false, CircuitState::Closed, Vec::new())?;

    let eligibility = evaluate_dispatch_eligibility(&bead, &context);

    assert!(!eligibility.ready_in_br);
    assert!(!eligibility.dispatchable_in_grove);
    assert!(eligibility.local_suppression_reasons.is_empty());
    Ok(())
}

#[test]
fn dispatch_label_suppresses_epics_and_tasks_the_same_way() -> TestResult {
    let bead = sample_bead(GroveBeadStatus::Ready, "epic", &["dispatch:no"], None, None)?;
    let context = sample_context(true, CircuitState::Closed, Vec::new())?;

    let eligibility = evaluate_dispatch_eligibility(&bead, &context);
    let reason_codes = suppression_codes(&eligibility);

    assert!(!eligibility.dispatchable_in_grove);
    assert!(reason_codes.contains(&"suppressed_by_label"));
    assert_eq!(reason_codes.len(), 1);
    Ok(())
}

#[test]
fn active_run_status_suppresses_dispatch() -> TestResult {
    let bead = sample_bead(GroveBeadStatus::Running, "task", &[], Some("run_123"), None)?;
    let context = sample_context(true, CircuitState::Closed, Vec::new())?;

    let eligibility = evaluate_dispatch_eligibility(&bead, &context);

    assert!(!eligibility.dispatchable_in_grove);
    assert!(suppression_codes(&eligibility).contains(&"active_run"));
    Ok(())
}

#[test]
fn checkpointed_status_is_dispatchable_for_resume() -> TestResult {
    let bead = sample_bead(
        GroveBeadStatus::Checkpointed,
        "task",
        &[],
        Some("run_456"),
        None,
    )?;
    let context = sample_context(true, CircuitState::Closed, Vec::new())?;

    let eligibility = evaluate_dispatch_eligibility(&bead, &context);

    assert!(eligibility.dispatchable_in_grove);
    assert!(!eligibility.has_local_suppressions());
    Ok(())
}

#[test]
fn retry_backoff_only_suppresses_while_timer_is_pending() -> TestResult {
    let blocked = sample_bead(
        GroveBeadStatus::WaitingToRetry,
        "task",
        &[],
        None,
        Some("2026-03-16T12:30:00Z"),
    )?;
    let expired = sample_bead(
        GroveBeadStatus::WaitingToRetry,
        "task",
        &[],
        None,
        Some("2026-03-16T11:30:00Z"),
    )?;
    let context = sample_context(true, CircuitState::Closed, Vec::new())?;

    let blocked_eligibility = evaluate_dispatch_eligibility(&blocked, &context);
    let expired_eligibility = evaluate_dispatch_eligibility(&expired, &context);

    assert!(suppression_codes(&blocked_eligibility).contains(&"retry_backoff_pending"));
    assert!(blocked_eligibility.has_local_suppressions());
    assert!(!expired_eligibility.has_local_suppressions());
    assert!(expired_eligibility.dispatchable_in_grove);
    Ok(())
}

#[test]
fn circuit_open_and_reservation_conflict_suppress_dispatch() -> TestResult {
    let bead = sample_bead(GroveBeadStatus::Ready, "task", &[], None, None)?;
    let conflict = ReservationConflict {
        requested_by_bead: BeadId::new("grove-1j9.5.10"),
        conflicting_bead: BeadId::new("grove-1j9.5.4"),
        requested_pattern: "crates/grove-kernel/**".into(),
        held_pattern: "crates/grove-kernel/src/lib.rs".into(),
        conflicting_run_id: Some(RunId::new("run_conflict")),
    };
    let context = sample_context(true, CircuitState::Open, vec![conflict])?;

    let eligibility = evaluate_dispatch_eligibility(&bead, &context);
    let reason_codes = suppression_codes(&eligibility);

    assert!(!eligibility.dispatchable_in_grove);
    assert!(reason_codes.contains(&"circuit_open"));
    assert!(reason_codes.contains(&"reservation_conflict"));
    Ok(())
}

#[test]
fn succeeded_and_failed_beads_are_not_dispatchable() -> TestResult {
    let succeeded = sample_bead(GroveBeadStatus::Succeeded, "task", &[], None, None)?;
    let failed = sample_bead(GroveBeadStatus::Failed, "task", &[], None, None)?;
    let context = sample_context(true, CircuitState::Closed, Vec::new())?;

    let succeeded_eligibility = evaluate_dispatch_eligibility(&succeeded, &context);
    let failed_eligibility = evaluate_dispatch_eligibility(&failed, &context);

    assert!(suppression_codes(&succeeded_eligibility).contains(&"already_succeeded"));
    assert!(suppression_codes(&failed_eligibility).contains(&"failed_awaiting_manual_retry"));
    Ok(())
}

#[test]
fn dependency_snapshot_sanity_detects_self_edges_and_duplicates() {
    let snapshot = BrDependencySnapshot {
        bead_id: BeadId::new("grove-1"),
        blocked_by: vec![
            BeadId::new("grove-parent"),
            BeadId::new("grove-1"),
            BeadId::new("grove-parent"),
        ],
        blocks: vec![
            BeadId::new("grove-1"),
            BeadId::new("grove-child"),
            BeadId::new("grove-child"),
        ],
        rows: Vec::new(),
    };

    let issues = validate_dependency_snapshot(&snapshot);
    let codes: Vec<_> = issues.iter().map(DependencySnapshotIssue::code).collect();

    assert!(codes.contains(&"self_blocked_by"));
    assert!(codes.contains(&"self_blocks"));
    assert!(codes.contains(&"duplicate_blocked_by"));
    assert!(codes.contains(&"duplicate_blocks"));
}

#[test]
fn dependency_snapshot_sanity_accepts_unique_non_self_edges() {
    let snapshot = BrDependencySnapshot {
        bead_id: BeadId::new("grove-1"),
        blocked_by: vec![BeadId::new("grove-parent")],
        blocks: vec![BeadId::new("grove-child")],
        rows: Vec::new(),
    };

    let sanity = DependencySnapshotSanity {
        snapshot: snapshot.clone(),
        issues: validate_dependency_snapshot(&snapshot),
    };

    assert!(sanity.is_sane());
    assert!(sanity.issues.is_empty());
}

fn sample_context(
    ready_in_br: bool,
    circuit_state: CircuitState,
    reservation_conflicts: Vec<ReservationConflict>,
) -> TestResult<DispatchEligibilityContext> {
    Ok(DispatchEligibilityContext {
        ready_in_br,
        circuit_state,
        reservation_conflicts,
        now: "2026-03-16T12:00:00Z".parse()?,
    })
}

fn sample_bead(
    grove_status: GroveBeadStatus,
    issue_type: &str,
    labels: &[&str],
    last_run_id: Option<&str>,
    retry_after: Option<&str>,
) -> TestResult<GroveBeadRecord> {
    let created_at: Timestamp = "2026-03-16T10:00:00Z".parse()?;
    let updated_at: Timestamp = "2026-03-16T11:00:00Z".parse()?;

    Ok(GroveBeadRecord {
        bead: BeadRef {
            id: BeadId::new("grove-1j9.5.10"),
            title: "dispatch policy".into(),
            description: None,
            priority: BeadPriority::P0,
            issue_type: issue_type.into(),
            br_status: "open".into(),
            assignee: None,
            labels: labels.iter().map(|label| (*label).to_owned()).collect(),
            created_at,
            updated_at,
        },
        grove_status,
        declared_paths: Vec::new(),
        metadata: Default::default(),
        last_run_id: last_run_id.map(RunId::new),
        retry_after: retry_after.map(str::parse).transpose()?,
        last_failure_class: None,
        last_failure_detail: None,
        circuit_breaker_state: None,
        synced_at: updated_at,
        runtime_updated_at: updated_at,
    })
}

#[test]
fn circuit_state_for_bead_uses_persisted_breaker_snapshot() -> TestResult {
    let mut bead = sample_bead(GroveBeadStatus::Ready, "task", &[], None, None)?;
    bead.circuit_breaker_state = Some(CircuitBreakerState {
        state: CircuitState::Open,
        no_progress_count: 3,
        same_error_count: 0,
        permission_denial_count: 0,
        last_error_fingerprint: Some("same-error".to_owned()),
        opened_at: Some("2026-03-16T12:00:00Z".parse()?),
    });
    assert_eq!(circuit_state_for_bead(&bead), CircuitState::Open);
    Ok(())
}

fn suppression_codes(eligibility: &DispatchEligibility) -> Vec<&'static str> {
    eligibility
        .local_suppression_reasons
        .iter()
        .map(LocalSuppressionReason::code)
        .collect()
}

fn insert_run_row(db: &Database, run_id: &str, bead_id: &str, status: &str) -> TestResult {
    db.connection().execute(
            "INSERT INTO task_runs(\
                id, bead_id, attempt_no, status, failure_class, failure_detail, started_at, ended_at, session_count, checkpoint_count, last_checkpoint_id\
             ) VALUES (?1, ?2, ?3, ?4, NULL, NULL, ?5, NULL, 0, 0, NULL)",
            rusqlite::params![run_id, bead_id, 1, status, "2026-03-16T11:00:00Z"],
        )?;
    Ok(())
}

#[test]
fn leader_lease_manager_round_trips_acquire_heartbeat_and_release() -> TestResult {
    let dir = tempfile::tempdir()?;
    let db_path = camino::Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
        .map_err(|_| std::io::Error::other("db path must be valid UTF-8"))?;
    let mut db = Database::open(&db_path)?;
    db.migrate()?;

    let config = LeaderLeaseConfig {
        owner_label: "worker-a".to_owned(),
        lease_ttl: chrono::Duration::seconds(30),
    };
    let acquired_at: Timestamp = "2026-03-16T12:00:00Z".parse()?;
    let heartbeat_at: Timestamp = "2026-03-16T12:00:05Z".parse()?;
    let release_at: Timestamp = "2026-03-16T12:00:10Z".parse()?;

    let lease = LeaderLeaseManager::acquire(&mut db, &config, None, acquired_at)?;
    assert_eq!(lease.owner_label, "worker-a");
    assert_eq!(lease.acquired_at, acquired_at);
    assert_eq!(
        lease.expires_at,
        "2026-03-16T12:00:30Z".parse::<Timestamp>()?
    );

    let heartbeat = require_some(
        LeaderLeaseManager::heartbeat(&mut db, &config, heartbeat_at)?,
        "heartbeat should refresh owned lease",
    )?;
    assert_eq!(heartbeat.heartbeat_at, heartbeat_at);
    assert_eq!(
        heartbeat.expires_at,
        "2026-03-16T12:00:35Z".parse::<Timestamp>()?
    );

    let released = require_some(
        LeaderLeaseManager::release(&mut db, "worker-a", release_at)?,
        "release should return the lease record",
    )?;
    assert_eq!(released.owner_label, "worker-a");
    assert!(db.active_leader_lease(&release_at)?.is_none());
    Ok(())
}

#[test]
fn leader_lease_manager_reports_contested_owner() -> TestResult {
    let dir = tempfile::tempdir()?;
    let db_path = camino::Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
        .map_err(|_| std::io::Error::other("db path must be valid UTF-8"))?;
    let mut db = Database::open(&db_path)?;
    db.migrate()?;

    let first = LeaderLeaseConfig {
        owner_label: "worker-a".to_owned(),
        lease_ttl: chrono::Duration::seconds(30),
    };
    let second = LeaderLeaseConfig {
        owner_label: "worker-b".to_owned(),
        lease_ttl: chrono::Duration::seconds(30),
    };
    let now: Timestamp = "2026-03-16T12:00:00Z".parse()?;

    LeaderLeaseManager::acquire(&mut db, &first, None, now)?;
    let error = LeaderLeaseManager::acquire(&mut db, &second, None, now)
        .expect_err("second owner should be rejected while first lease is active");

    assert_eq!(
        error,
        LeaderLeaseAcquireError::Contested {
            owner_label: "worker-a".to_owned(),
        }
    );
    Ok(())
}

#[test]
fn startup_reconciliation_marks_active_runs_retryable_and_releases_stale_reservations() -> TestResult
{
    let dir = tempfile::tempdir()?;
    let db_path = camino::Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
        .map_err(|_| std::io::Error::other("db path must be valid UTF-8"))?;
    let mut db = Database::open(&db_path)?;
    db.migrate()?;
    insert_bead_cache_row(&db, "grove-recover", "Recover bead")?;
    insert_run_row(&db, "run-recover", "grove-recover", "Active")?;
    db.connection().execute(
        "INSERT INTO reservations(\
                bead_id, run_id, path_pattern, exclusive, reason, expires_at, released_at\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)",
        rusqlite::params![
            "grove-recover",
            "run-recover",
            "crates/grove-kernel/src/lib.rs",
            1,
            "startup recovery test",
            "2099-03-16T12:30:00Z",
        ],
    )?;

    let now: Timestamp = "2026-03-16T12:05:00Z".parse()?;
    let report = reconcile_startup_state(&mut db, now)?;

    assert_eq!(report.interrupted_runs.len(), 1);
    assert_eq!(report.interrupted_runs[0].bead_id.as_str(), "grove-recover");
    assert_eq!(
        report.interrupted_runs[0].run.status,
        RunStatus::WaitingToRetry
    );
    assert_eq!(
        report.interrupted_runs[0].run.failure_class,
        Some(FailureClass::Interrupted)
    );
    assert_eq!(report.reservations.recovered.len(), 1);
    assert_eq!(
        report.reservations.recovered[0].reservation.path_pattern,
        "crates/grove-kernel/src/lib.rs"
    );
    assert!(report.reservations.expired.is_empty());

    let bead = require_some(
        db.get_bead_record(&BeadId::new("grove-recover"))?,
        "runtime bead should exist",
    )?;
    assert_eq!(bead.grove_status, GroveBeadStatus::WaitingToRetry);
    assert_eq!(bead.last_failure_class, Some(FailureClass::Interrupted));
    assert!(db.list_active_reservations_at(&now)?.is_empty());
    Ok(())
}

#[test]
fn acquire_startup_coordinator_returns_leader_and_recovery_report() -> TestResult {
    let dir = tempfile::tempdir()?;
    let db_path = camino::Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
        .map_err(|_| std::io::Error::other("db path must be valid UTF-8"))?;
    let mut db = Database::open(&db_path)?;
    db.migrate()?;
    insert_bead_cache_row(&db, "grove-startup", "Startup bead")?;
    insert_run_row(&db, "run-startup", "grove-startup", "Active")?;

    let config = LeaderLeaseConfig {
        owner_label: "coordinator-1".to_owned(),
        lease_ttl: chrono::Duration::seconds(45),
    };
    let now: Timestamp = "2026-03-16T12:00:00Z".parse()?;
    let state =
        acquire_startup_coordinator(&mut db, &config, Some(&RunId::new("run-startup")), now)?;

    assert_eq!(state.leader.owner_label, "coordinator-1");
    assert_eq!(
        state.leader.run_id.as_ref().map(RunId::as_str),
        Some("run-startup")
    );
    assert_eq!(state.recovery.interrupted_runs.len(), 1);
    assert_eq!(
        state.recovery.interrupted_runs[0].run.id.as_str(),
        "run-startup"
    );
    assert!(state.recovery.reservations.recovered.is_empty());
    Ok(())
}

#[test]
fn acquire_startup_coordinator_releases_lease_when_recovery_fails() -> TestResult {
    let dir = tempfile::tempdir()?;
    let db_path = camino::Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
        .map_err(|_| std::io::Error::other("db path must be valid UTF-8"))?;
    let mut db = Database::open(&db_path)?;
    db.migrate()?;

    let config = LeaderLeaseConfig {
        owner_label: "coordinator-1".to_owned(),
        lease_ttl: chrono::Duration::seconds(45),
    };
    let now: Timestamp = "2026-03-16T12:00:00Z".parse()?;
    let missing_run = RunId::new("run-missing");
    let error = acquire_startup_coordinator(&mut db, &config, Some(&missing_run), now)
        .expect_err("missing run should fail startup acquisition");

    assert_eq!(
        error,
        LeaderLeaseAcquireError::Contested {
            owner_label: "run run-missing does not exist".to_owned(),
        }
    );
    assert!(db.active_leader_lease(&now)?.is_none());
    Ok(())
}

#[cfg(unix)]
fn write_fake_claude_script(path: &std::path::Path) -> TestResult {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    let script = r#"#!/bin/sh
printf '%b' "$STDOUT_SCRIPT"
printf '%b' "$STDERR_SCRIPT" >&2
exit "${EXIT_CODE:-0}"
"#;
    let temp_path = path.with_extension("tmp");
    fs::write(&temp_path, script)?;
    let mut permissions = fs::metadata(&temp_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&temp_path, permissions)?;
    fs::rename(&temp_path, path)?;
    Ok(())
}

fn insert_bead_cache_row(db: &Database, bead_id: &str, title: &str) -> TestResult {
    db.connection().execute(
            "INSERT INTO bead_cache(\
                bead_id, title, description, priority, issue_type, status, assignee,\
                labels_json, parent_ids_json, dependency_ids_json, dependent_ids_json, raw_json, synced_at\
             ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, NULL, '[]', '[]', '[]', '[]', '{}', ?6)",
            rusqlite::params![bead_id, title, 0, "task", "open", "2026-03-16T10:00:00Z"],
        )?;
    Ok(())
}

#[cfg(unix)]
fn sample_session_request(
    workspace_dir: camino::Utf8PathBuf,
) -> grove_session::SingleTaskSessionRequest {
    grove_session::SingleTaskSessionRequest {
        bead_id: BeadId::new("grove-life"),
        run_id: RunId::new("run-life"),
        session_id: grove_types::SessionId::new("ses-life"),
        provider: grove_types::RuntimeProvider::Claude,
        prompt_id: grove_types::PromptId::new("prompt-life"),
        task_title: "Persist runtime lifecycle".to_owned(),
        task_description: "Wire session lifecycle into the runtime DB.".to_owned(),
        startup_prompt: None,
        contract: grove_types::ExecutionContract::SingleTask,
        model: "sonnet".to_owned(),
        working_dir: workspace_dir,
        transcript_path: camino::Utf8PathBuf::from(".grove/transcripts/grove-life/ses-life.jsonl"),
        prompt_manifest_path: camino::Utf8PathBuf::from(".grove/prompts/prompt-life.json"),
        timeout: std::time::Duration::from_secs(60),
        exit_policy: grove_session::ExitPolicy::default(),
        context_monitor: grove_session::ContextMonitor::new(0.7, 0.82, 0.9, 16_000),
        reservation_hints: vec!["crates/grove-kernel/src/lib.rs".to_owned()],
        parent_handoffs: vec!["Kernel should own runtime persistence glue.".to_owned()],
        checkpoint: None,
        previous_failure_class: None,
        previous_outcome: None,
        rescue_card: None,
        retry_delta_summary: None,
        retrieval_query: None,
        token_budget: Some(2_000),
        ordinal_in_run: 1,
        archive_bundle: None,
        playbook_rules: Vec::new(),
        env: Vec::new(),
        shutdown: grove_session::SessionShutdownConfig::default(),
        escalation_tier: grove_types::EscalationTier::FirstAttempt,
        mutation_strategy: None,
        idle_grace_period: std::time::Duration::from_secs(300),
    }
}

#[cfg(unix)]
#[test]
fn persisted_runner_records_successful_run_and_session() -> TestResult {
    use std::{fs, io};
    use tempfile::tempdir;

    let dir = tempdir()?;
    let workspace_dir = dir.path().join("workspace");
    fs::create_dir_all(&workspace_dir)?;
    let workspace_dir = camino::Utf8PathBuf::from_path_buf(workspace_dir)
        .map_err(|_| io::Error::other("workspace dir must be valid UTF-8"))?;
    let db_path = camino::Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
        .map_err(|_| io::Error::other("db path must be valid UTF-8"))?;

    let mut db = Database::open(&db_path)?;
    db.migrate()?;
    insert_bead_cache_row(&db, "grove-life", "Lifecycle bead")?;

    let script_path = dir.path().join("fake-claude");
    write_fake_claude_script(&script_path)?;
    let backend = grove_session::CliClaudeBackend::new(script_path.to_string_lossy().into_owned());

    let mut request = sample_session_request(workspace_dir);
    request.env = vec![
        (
            "STDOUT_SCRIPT".to_owned(),
            concat!(
                "working through the task\n",
                "GROVE_RESULT: runtime persistence wired\n",
                "GROVE_ARTIFACTS: [\"crates/grove-kernel/src/lib.rs\"]\n",
                "GROVE_EXIT: true\n",
                "all tasks complete\n",
                "implementation complete\n"
            )
            .to_owned(),
        ),
        ("STDERR_SCRIPT".to_owned(), String::new()),
        ("EXIT_CODE".to_owned(), "0".to_owned()),
    ];

    let persisted = execute_persisted_single_task_session(
        &mut db,
        &backend,
        request,
        1,
        &GroveConfig::default(),
    )?;

    assert_eq!(persisted.run.status, RunStatus::Succeeded);
    assert_eq!(persisted.session.session.status, SessionStatus::Completed);
    assert!(persisted.checkpoint.is_none());
    assert_eq!(
        persisted
            .handoff
            .as_ref()
            .map(|handoff| handoff.summary.as_str()),
        Some("runtime persistence wired")
    );
    assert_eq!(
        persisted
            .handoff
            .as_ref()
            .map(|handoff| handoff.artifacts.clone()),
        Some(vec!["crates/grove-kernel/src/lib.rs".to_owned()])
    );
    assert_eq!(
        require_some(
            db.latest_session_for_run(&RunId::new("run-life"))?,
            "session should persist",
        )?
        .status,
        SessionStatus::Completed
    );
    assert_eq!(
        require_some(
            db.handoff_for_bead(&BeadId::new("grove-life"))?,
            "handoff should persist",
        )?
        .summary,
        "runtime persistence wired"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn persisted_runner_writes_trace_log_for_successful_session() -> TestResult {
    use std::{fs, io};
    use tempfile::tempdir;

    let dir = tempdir()?;
    let workspace_dir = dir.path().join("workspace");
    fs::create_dir_all(&workspace_dir)?;
    let workspace_dir = camino::Utf8PathBuf::from_path_buf(workspace_dir)
        .map_err(|_| io::Error::other("workspace dir must be valid UTF-8"))?;
    let db_path = camino::Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
        .map_err(|_| io::Error::other("db path must be valid UTF-8"))?;

    init_trace_logging(&workspace_dir, true)?;

    let mut db = Database::open(&db_path)?;
    db.migrate()?;
    insert_bead_cache_row(&db, "grove-life", "Lifecycle bead")?;

    let script_path = dir.path().join("fake-claude");
    write_fake_claude_script(&script_path)?;
    let backend = grove_session::CliClaudeBackend::new(script_path.to_string_lossy().into_owned());

    let mut request = sample_session_request(workspace_dir.clone());
    request.env = vec![
        (
            "STDOUT_SCRIPT".to_owned(),
            concat!(
                "working through the task\n",
                "GROVE_RESULT: runtime persistence wired\n",
                "GROVE_ARTIFACTS: [\"crates/grove-kernel/src/lib.rs\"]\n",
                "GROVE_EXIT: true\n",
                "all tasks complete\n",
                "implementation complete\n"
            )
            .to_owned(),
        ),
        ("STDERR_SCRIPT".to_owned(), String::new()),
        ("EXIT_CODE".to_owned(), "0".to_owned()),
    ];

    let _persisted = execute_persisted_single_task_session(
        &mut db,
        &backend,
        request,
        1,
        &GroveConfig::default(),
    )?;

    let lines = read_trace_log_lines(&workspace_dir)?;
    assert!(lines.iter().any(|line| line["event"] == "session.started"));
    assert!(lines.iter().any(|line| line["event"] == "session.finished"));
    assert!(
        lines
            .iter()
            .any(|line| line["event"] == "session.run_result")
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn persisted_runner_records_checkpoint_and_checkpointed_run() -> TestResult {
    use std::{fs, io};
    use tempfile::tempdir;

    let dir = tempdir()?;
    let workspace_dir = dir.path().join("workspace");
    fs::create_dir_all(&workspace_dir)?;
    let workspace_dir = camino::Utf8PathBuf::from_path_buf(workspace_dir)
        .map_err(|_| io::Error::other("workspace dir must be valid UTF-8"))?;
    let db_path = camino::Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
        .map_err(|_| io::Error::other("db path must be valid UTF-8"))?;

    let mut db = Database::open(&db_path)?;
    db.migrate()?;
    insert_bead_cache_row(&db, "grove-life", "Lifecycle bead")?;

    let script_path = dir.path().join("fake-claude");
    write_fake_claude_script(&script_path)?;
    let backend = grove_session::CliClaudeBackend::new(script_path.to_string_lossy().into_owned());

    let mut request = sample_session_request(workspace_dir.clone());
    request.env = vec![
            (
                "STDOUT_SCRIPT".to_owned(),
                concat!(
                    "GROVE_RESULT: checkpoint before rotation\n",
                    "GROVE_EXIT: true\n",
                    "all tasks complete\n",
                    "implementation complete\n",
                    "GROVE_CHECKPOINT: {\"progress\":\"halfway\",\"next_step\":\"finish wiring\",\"context\":{},\"open_questions\":[],\"claimed_paths\":[\"crates/grove-kernel/src/lib.rs\"]}\n"
                )
                .to_owned(),
            ),
            ("STDERR_SCRIPT".to_owned(), String::new()),
            ("EXIT_CODE".to_owned(), "0".to_owned()),
        ];

    let persisted = execute_persisted_single_task_session(
        &mut db,
        &backend,
        request,
        1,
        &GroveConfig::default(),
    )?;

    assert_eq!(persisted.run.status, RunStatus::Checkpointed);
    assert_eq!(
        persisted.session.session.status,
        SessionStatus::Checkpointed
    );
    assert_eq!(
        persisted.checkpoint.as_ref().map(|c| c.next_step.as_str()),
        Some("finish wiring")
    );
    assert_eq!(
        require_some(
            db.latest_checkpoint_for_bead(&BeadId::new("grove-life"))?,
            "checkpoint should persist",
        )?
        .next_step,
        "finish wiring"
    );
    let checkpoint_path = workspace_dir
        .as_std_path()
        .join(".grove/checkpoints/grove-life/chk-run-life-1.json");
    assert!(
        checkpoint_path.exists(),
        "checkpoint file should be written"
    );
    let checkpoint_json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&checkpoint_path)?)?;
    assert_eq!(checkpoint_json["id"], "chk-run-life-1");
    assert_eq!(checkpoint_json["bead_id"], "grove-life");
    assert_eq!(checkpoint_json["run_id"], "run-life");
    assert_eq!(checkpoint_json["session_id"], "ses-life");
    assert_eq!(checkpoint_json["next_step"], "finish wiring");
    assert_eq!(checkpoint_json["resume_generation"], 1);
    Ok(())
}

#[cfg(unix)]
#[test]
fn checkpoint_file_write_failure_preserves_checkpointed_runtime_state() -> TestResult {
    use std::{fs, io};
    use tempfile::tempdir;

    let dir = tempdir()?;
    let workspace_dir = dir.path().join("workspace");
    fs::create_dir_all(&workspace_dir)?;
    let checkpoints_parent = workspace_dir.join(".grove/checkpoints");
    fs::create_dir_all(&checkpoints_parent)?;
    fs::write(checkpoints_parent.join("grove-life"), b"occupied")?;
    let workspace_dir = camino::Utf8PathBuf::from_path_buf(workspace_dir)
        .map_err(|_| io::Error::other("workspace dir must be valid UTF-8"))?;
    let db_path = camino::Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
        .map_err(|_| io::Error::other("db path must be valid UTF-8"))?;

    let mut db = Database::open(&db_path)?;
    db.migrate()?;
    insert_bead_cache_row(&db, "grove-life", "Lifecycle bead")?;

    let script_path = dir.path().join("fake-claude");
    write_fake_claude_script(&script_path)?;
    let backend = grove_session::CliClaudeBackend::new(script_path.to_string_lossy().into_owned());

    let mut request = sample_session_request(workspace_dir.clone());
    request.env = vec![
            (
                "STDOUT_SCRIPT".to_owned(),
                concat!(
                    "GROVE_RESULT: checkpoint before rotation\n",
                    "GROVE_EXIT: true\n",
                    "all tasks complete\n",
                    "implementation complete\n",
                    "GROVE_CHECKPOINT: {\"progress\":\"halfway\",\"next_step\":\"finish wiring\",\"context\":{},\"open_questions\":[],\"claimed_paths\":[\"crates/grove-kernel/src/lib.rs\"]}\n"
                )
                .to_owned(),
            ),
            ("STDERR_SCRIPT".to_owned(), String::new()),
            ("EXIT_CODE".to_owned(), "0".to_owned()),
        ];

    let error = execute_persisted_single_task_session(
        &mut db,
        &backend,
        request,
        1,
        &GroveConfig::default(),
    )
    .expect_err("checkpoint file write should fail");
    assert!(
        error
            .to_string()
            .contains("failed to persist checkpoint file")
    );

    let bead = require_some(
        db.get_bead_record(&BeadId::new("grove-life"))?,
        "bead runtime should persist",
    )?;
    assert_eq!(bead.grove_status, GroveBeadStatus::Checkpointed);

    let run = require_some(
        db.list_task_runs_for_bead(&BeadId::new("grove-life"))?
            .into_iter()
            .next(),
        "run should persist",
    )?;
    assert_eq!(run.status, RunStatus::Checkpointed);

    let checkpoint = require_some(
        db.latest_checkpoint_for_bead(&BeadId::new("grove-life"))?,
        "checkpoint row should persist",
    )?;
    assert_eq!(checkpoint.next_step, "finish wiring");

    let run = require_some(
        db.list_task_runs_for_bead(&BeadId::new("grove-life"))?
            .into_iter()
            .next(),
        "run should persist",
    )?;
    assert!(
        run.failure_detail
            .as_deref()
            .is_some_and(|detail| detail.contains("failed to persist checkpoint file"))
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn persisted_runner_writes_fallback_checkpoint_for_crashed_run_without_protocol_checkpoint()
-> TestResult {
    use std::{fs, io};
    use tempfile::tempdir;

    let dir = tempdir()?;
    let workspace_dir = dir.path().join("workspace");
    fs::create_dir_all(&workspace_dir)?;
    let workspace_dir = camino::Utf8PathBuf::from_path_buf(workspace_dir)
        .map_err(|_| io::Error::other("workspace dir must be valid UTF-8"))?;
    let db_path = camino::Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
        .map_err(|_| io::Error::other("db path must be valid UTF-8"))?;

    let mut db = Database::open(&db_path)?;
    db.migrate()?;
    insert_bead_cache_row(&db, "grove-life", "Lifecycle bead")?;

    let script_path = dir.path().join("fake-claude-crash");
    write_fake_claude_script(&script_path)?;
    let backend = grove_session::CliClaudeBackend::new(script_path.to_string_lossy().into_owned());

    let mut request = sample_session_request(workspace_dir.clone());
    request.env = vec![
        ("STDOUT_SCRIPT".to_owned(), "starting work\n".to_owned()),
        (
            "STDERR_SCRIPT".to_owned(),
            "API Error: 400 The image data you provided does not represent a valid image\n"
                .to_owned(),
        ),
        ("EXIT_CODE".to_owned(), "1".to_owned()),
    ];

    let persisted = execute_persisted_single_task_session(
        &mut db,
        &backend,
        request,
        1,
        &GroveConfig::default(),
    )?;

    assert_eq!(persisted.run.status, RunStatus::Failed);
    assert_eq!(
        persisted.run.failure_class,
        Some(FailureClass::ClaudeCrashed)
    );
    let checkpoint = require_some(
        persisted.checkpoint,
        "fallback checkpoint should be written for crash without GROVE_CHECKPOINT",
    )?;
    assert!(checkpoint.id.as_str().contains("fallback"));
    assert!(
        checkpoint
            .progress
            .contains("Synthetic fallback checkpoint")
    );
    assert!(
        checkpoint
            .next_step
            .contains("Resume from the transcript tail")
    );
    let checkpoint_path = workspace_dir.as_std_path().join(format!(
        ".grove/checkpoints/grove-life/{}.json",
        checkpoint.id.as_str()
    ));
    assert!(
        checkpoint_path.exists(),
        "fallback checkpoint file should be written"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn persisted_runner_records_unknown_failure_with_progress_as_waiting_to_retry() -> TestResult {
    use std::{fs, io};
    use tempfile::tempdir;

    let dir = tempdir()?;
    let workspace_dir = dir.path().join("workspace");
    fs::create_dir_all(&workspace_dir)?;
    let workspace_dir = camino::Utf8PathBuf::from_path_buf(workspace_dir)
        .map_err(|_| io::Error::other("workspace dir must be valid UTF-8"))?;
    let db_path = camino::Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
        .map_err(|_| io::Error::other("db path must be valid UTF-8"))?;

    let mut db = Database::open(&db_path)?;
    db.migrate()?;
    insert_bead_cache_row(&db, "grove-life", "Lifecycle bead")?;

    let script_path = dir.path().join("fake-claude");
    write_fake_claude_script(&script_path)?;
    let backend = grove_session::CliClaudeBackend::new(script_path.to_string_lossy().into_owned());

    let mut request = sample_session_request(workspace_dir);
    request.env = vec![
        (
            "STDOUT_SCRIPT".to_owned(),
            "Implemented the plan file and requested approval.\n".to_owned(),
        ),
        ("STDERR_SCRIPT".to_owned(), String::new()),
        ("EXIT_CODE".to_owned(), "0".to_owned()),
    ];

    let persisted = execute_persisted_single_task_session(
        &mut db,
        &backend,
        request,
        1,
        &GroveConfig::default(),
    )?;

    assert_eq!(persisted.run.status, RunStatus::WaitingToRetry);
    assert_eq!(persisted.run.failure_class, Some(FailureClass::NoProgress));
    assert_eq!(
        persisted.session.session.status,
        SessionStatus::UnknownFailure
    );
    assert_eq!(
        persisted.session.session.stop_reason,
        Some(grove_types::StopReason::Unknown)
    );
    let bead = require_some(
        db.get_bead_record(&BeadId::new("grove-life"))?,
        "bead runtime should persist",
    )?;
    assert_eq!(bead.grove_status, GroveBeadStatus::Checkpointed);
    assert_eq!(bead.last_failure_class, None);
    assert!(persisted.checkpoint.is_some());
    Ok(())
}

#[cfg(unix)]
#[test]
fn persisted_runner_records_signaled_unknown_failure_as_waiting_to_retry() -> TestResult {
    use std::{fs, io};
    use tempfile::tempdir;

    let dir = tempdir()?;
    let workspace_dir = dir.path().join("workspace");
    fs::create_dir_all(&workspace_dir)?;
    let workspace_dir = camino::Utf8PathBuf::from_path_buf(workspace_dir)
        .map_err(|_| io::Error::other("workspace dir must be valid UTF-8"))?;
    let db_path = camino::Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
        .map_err(|_| io::Error::other("db path must be valid UTF-8"))?;

    let mut db = Database::open(&db_path)?;
    db.migrate()?;
    insert_bead_cache_row(&db, "grove-life", "Lifecycle bead")?;

    let script_path = dir.path().join("fake-claude-signal");
    fs::write(&script_path, "#!/bin/sh\nkill -TERM $$\n")?;
    let mut permissions = fs::metadata(&script_path)?.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o755);
    }
    fs::set_permissions(&script_path, permissions)?;
    let backend = grove_session::CliClaudeBackend::new(script_path.to_string_lossy().into_owned());

    let request = sample_session_request(workspace_dir);
    let persisted = execute_persisted_single_task_session(
        &mut db,
        &backend,
        request,
        1,
        &GroveConfig::default(),
    )?;

    assert_eq!(persisted.run.status, RunStatus::WaitingToRetry);
    assert_eq!(persisted.run.failure_class, Some(FailureClass::NoProgress));
    assert_eq!(
        persisted.session.session.status,
        SessionStatus::UnknownFailure
    );
    assert_eq!(persisted.session.session.exit_code, None);
    let bead = require_some(
        db.get_bead_record(&BeadId::new("grove-life"))?,
        "bead runtime should persist",
    )?;
    assert_eq!(bead.grove_status, GroveBeadStatus::Checkpointed);
    assert_eq!(bead.last_failure_class, None);
    assert!(persisted.checkpoint.is_some());
    Ok(())
}

#[cfg(unix)]
#[test]
fn persisted_runner_records_forced_kill_as_waiting_to_retry() -> TestResult {
    use std::{
        fs, io,
        sync::{Arc, atomic::AtomicBool},
    };
    use tempfile::tempdir;

    let dir = tempdir()?;
    let workspace_dir = dir.path().join("workspace");
    fs::create_dir_all(&workspace_dir)?;
    let workspace_dir = camino::Utf8PathBuf::from_path_buf(workspace_dir)
        .map_err(|_| io::Error::other("workspace dir must be valid UTF-8"))?;
    let db_path = camino::Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
        .map_err(|_| io::Error::other("db path must be valid UTF-8"))?;

    let mut db = Database::open(&db_path)?;
    db.migrate()?;
    insert_bead_cache_row(&db, "grove-life", "Lifecycle bead")?;

    let script_path = dir.path().join("fake-claude");
    fs::write(&script_path, "#!/bin/sh\nsleep 1\n")?;
    let mut permissions = fs::metadata(&script_path)?.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o755);
    }
    fs::set_permissions(&script_path, permissions)?;
    let backend = grove_session::CliClaudeBackend::new(script_path.to_string_lossy().into_owned());

    let shutdown = Arc::new(AtomicBool::new(true));
    let mut request = sample_session_request(workspace_dir);
    request.shutdown = grove_session::SessionShutdownConfig {
        signal: Some(shutdown),
        grace_period: Some(std::time::Duration::from_millis(0)),
    };

    let persisted = execute_persisted_single_task_session(
        &mut db,
        &backend,
        request,
        1,
        &GroveConfig::default(),
    )?;

    assert_eq!(persisted.run.status, RunStatus::WaitingToRetry);
    assert_eq!(persisted.run.failure_class, Some(FailureClass::Interrupted));
    let bead = require_some(
        db.get_bead_record(&BeadId::new("grove-life"))?,
        "bead runtime should persist",
    )?;
    assert_eq!(bead.grove_status, GroveBeadStatus::Checkpointed);
    assert_eq!(bead.last_failure_class, None);
    assert!(persisted.checkpoint.is_some());
    Ok(())
}

#[cfg(unix)]
#[test]
fn persisted_runner_records_rate_limit_as_waiting_to_retry() -> TestResult {
    use std::{fs, io};
    use tempfile::tempdir;

    let dir = tempdir()?;
    let workspace_dir = dir.path().join("workspace");
    fs::create_dir_all(&workspace_dir)?;
    let workspace_dir = camino::Utf8PathBuf::from_path_buf(workspace_dir)
        .map_err(|_| io::Error::other("workspace dir must be valid UTF-8"))?;
    let db_path = camino::Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
        .map_err(|_| io::Error::other("db path must be valid UTF-8"))?;

    let mut db = Database::open(&db_path)?;
    db.migrate()?;
    insert_bead_cache_row(&db, "grove-life", "Lifecycle bead")?;

    let script_path = dir.path().join("fake-claude");
    write_fake_claude_script(&script_path)?;
    let backend = grove_session::CliClaudeBackend::new(script_path.to_string_lossy().into_owned());

    let mut request = sample_session_request(workspace_dir);
    request.env = vec![
        (
            "STDOUT_SCRIPT".to_owned(),
            "rate limit exceeded by upstream\n".to_owned(),
        ),
        (
            "STDERR_SCRIPT".to_owned(),
            "ratelimit retry window still active\n".to_owned(),
        ),
        ("EXIT_CODE".to_owned(), "1".to_owned()),
    ];

    let persisted = execute_persisted_single_task_session(
        &mut db,
        &backend,
        request,
        1,
        &GroveConfig::default(),
    )?;

    assert_eq!(persisted.run.status, RunStatus::WaitingToRetry);
    assert_eq!(persisted.run.failure_class, Some(FailureClass::RateLimit));
    assert_eq!(persisted.session.session.status, SessionStatus::RateLimited);
    assert!(persisted.handoff.is_none());
    assert_eq!(
        require_some(
            db.get_bead_record(&BeadId::new("grove-life"))?,
            "bead runtime should persist",
        )?
        .retry_after,
        persisted.run.ended_at
    );
    Ok(())
}

#[test]
fn parent_handoff_summaries_include_latest_parent_context() -> TestResult {
    let dir = tempfile::tempdir()?;
    let db_path = camino::Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
        .map_err(|_| std::io::Error::other("db path must be valid UTF-8"))?;
    let mut db = Database::open(&db_path)?;
    db.migrate()?;

    insert_bead_cache_row(&db, "grove-parent", "Parent bead")?;
    insert_bead_cache_row(&db, "grove-child", "Child bead")?;

    db.connection().execute(
            "INSERT INTO task_runs(\
                id, bead_id, attempt_no, status, failure_class, failure_detail, started_at, ended_at, session_count, checkpoint_count, last_checkpoint_id\
             ) VALUES (?1, ?2, ?3, ?4, NULL, NULL, ?5, ?6, ?7, ?8, NULL)",
            rusqlite::params![
                "run-parent",
                "grove-parent",
                1,
                "Succeeded",
                "2026-03-16T11:00:00Z",
                "2026-03-16T11:10:00Z",
                1,
                0,
            ],
        )?;
    db.connection().execute(
            "INSERT INTO bead_dependencies(parent_id, child_id, relation_type, synced_at) VALUES (?1, ?2, 'blocks', ?3)",
            rusqlite::params!["grove-parent", "grove-child", "2026-03-16T11:11:00Z"],
        )?;

    db.write_handoff(HandoffWriteInput {
        bead_id: BeadId::new("grove-parent"),
        run_id: RunId::new("run-parent"),
        summary: "parent finished the schema layer".to_owned(),
        artifacts: vec!["crates/grove-db/src/lib.rs".to_owned()],
        lessons: vec!["Validate schema writes before unblock".to_owned()],
        decisions: vec!["Keep br as the dependency authority".to_owned()],
        warnings: vec!["Mirror flow still pending".to_owned()],
        completed_at: "2026-03-16T11:12:00Z".parse()?,
    })?;

    let summaries = parent_handoff_summaries(&db, &BeadId::new("grove-child"))?;
    assert_eq!(summaries.len(), 1);
    assert!(summaries[0].contains(
        "Parent grove-parent (run run-parent) prepared this task: parent finished the schema layer"
    ));
    assert!(summaries[0].contains("Artifacts: crates/grove-db/src/lib.rs"));
    assert!(summaries[0].contains("Decisions: Keep br as the dependency authority"));
    assert!(summaries[0].contains("Lessons: Validate schema writes before unblock"));
    assert!(summaries[0].contains("Warnings: Mirror flow still pending"));
    Ok(())
}
