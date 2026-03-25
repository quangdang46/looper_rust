#![allow(clippy::unwrap_used, clippy::expect_used)]

// Phase 3 Acceptance Tests
//
// This test suite covers Phase 3 orchestration requirements:
// 1. Graceful Shutdown & Stop Reasons
// 2. Parallel Orchestration Safety (Leader Leases & Reservations)
// 3. Crash Recovery (Reconciling interrupted runs)
// 4. Mirror-pending Behavior
// 5. Run Metrics Aggregation & Post-mortem Analysis

use camino::Utf8PathBuf;
use chrono::{DateTime, Utc};
use grove_config::GroveConfig;
use grove_db::{Database, RunFinishInput, RunStartInput};
use grove_kernel::{
    DispatchExitReason, ShutdownSignal, execute_persisted_single_task_session, init_trace_logging,
};
use grove_session::{
    CliClaudeBackend, ContextMonitor, ExitPolicy, SessionLifecycleHooks, SessionShutdownConfig,
    SingleTaskSessionRequest, execute_single_task_session_with_hooks,
};
use grove_types::{
    AgentActivity, BeadId, ClaudeSessionRecord, CoordinatorStopReason, EscalationTier, EventKind,
    ExecutionContract, FailureClass, PromptId, RunId, RunStatus, SessionId,
};
use std::{fs, io, os::unix::fs::PermissionsExt, sync::Mutex, time::Duration};
use tempfile::tempdir;

// NOTE: We rely on `grove_kernel` unit tests for extensive DB interactions,
// but these acceptance suites map the behavioral promises of Phase 3.

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn write_fake_claude_script(path: &std::path::Path) -> TestResult {
    let script = r#"#!/bin/sh
if [ -n "$PRE_OUTPUT_SLEEP_SECS" ]; then
    sleep "$PRE_OUTPUT_SLEEP_SECS"
fi
printf '%b' "$STDOUT_SCRIPT"
printf '%b' "$STDERR_SCRIPT" >&2
exit "${EXIT_CODE:-0}"
"#;
    fs::write(path, script)?;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

fn sample_request(workspace_dir: Utf8PathBuf) -> SingleTaskSessionRequest {
    SingleTaskSessionRequest {
        bead_id: BeadId::new("grove-244"),
        run_id: RunId::new("run-activity-proof"),
        session_id: SessionId::new("ses-activity-proof"),
        prompt_id: PromptId::new("prompt-activity-proof"),
        task_title: "Prove live activity transitions".to_owned(),
        task_description: "Acceptance proof for observable activity transitions.".to_owned(),
        startup_prompt: None,
        contract: ExecutionContract::SingleTask,
        model: "sonnet".to_owned(),
        working_dir: workspace_dir,
        transcript_path: Utf8PathBuf::from(".grove/transcripts/grove-244/ses-activity-proof.jsonl"),
        prompt_manifest_path: Utf8PathBuf::from(".grove/prompts/prompt-activity-proof.json"),
        timeout: Duration::from_secs(60),
        exit_policy: ExitPolicy::default(),
        context_monitor: ContextMonitor::new(0.7, 0.82, 0.9, 16_000),
        reservation_hints: vec!["crates/grove-cli/tests/phase3_acceptance.rs".to_owned()],
        parent_handoffs: vec!["Phase 3 needs live activity proof.".to_owned()],
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
        shutdown: SessionShutdownConfig::default(),
        escalation_tier: EscalationTier::FirstAttempt,
        mutation_strategy: None,
        idle_grace_period: Duration::from_secs(300),
    }
}

type ActivityChange = (AgentActivity, Option<String>, DateTime<Utc>);

#[derive(Default)]
struct ActivityRecordingHooks {
    changes: Mutex<Vec<ActivityChange>>,
    started: Mutex<Vec<ClaudeSessionRecord>>,
}

impl SessionLifecycleHooks for ActivityRecordingHooks {
    fn on_session_started(&mut self, session: &ClaudeSessionRecord) -> anyhow::Result<()> {
        self.started
            .lock()
            .expect("lock started")
            .push(session.clone());
        Ok(())
    }

    fn on_activity_changed(
        &mut self,
        activity: AgentActivity,
        detail: Option<&str>,
        at: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        self.changes.lock().expect("lock activity changes").push((
            activity,
            detail.map(str::to_owned),
            at,
        ));
        Ok(())
    }
}

#[test]
fn shutdown_signal_translates_to_durable_stop_reason() {
    let signal = ShutdownSignal::new();
    signal.trigger();

    assert!(
        signal.is_triggered(),
        "Shutdown signal should register as triggered."
    );

    let exit_reason = DispatchExitReason::ShutdownRequested;
    let stop_reason = exit_reason.to_stop_reason();

    assert_eq!(
        stop_reason,
        CoordinatorStopReason::UserStopped,
        "ShutdownRequested exit must map to a clean UserStopped durable reason."
    );
    assert!(stop_reason.is_user_initiated());
    assert!(stop_reason.is_clean());
}

#[test]
fn empty_queue_maps_to_clean_stop_reason() {
    let exit_reason = DispatchExitReason::QueueEmpty;
    let stop_reason = exit_reason.to_stop_reason();

    assert_eq!(stop_reason, CoordinatorStopReason::QueueEmpty);
    assert!(stop_reason.is_clean());
    assert!(!stop_reason.is_user_initiated());
}

#[test]
fn blocked_queue_maps_to_clean_stop_reason() {
    let exit_reason = DispatchExitReason::DispatchBlocked;
    let stop_reason = exit_reason.to_stop_reason();

    assert_eq!(stop_reason, CoordinatorStopReason::DispatchBlocked);
    assert!(stop_reason.is_clean());
    assert!(!stop_reason.is_user_initiated());
}

#[test]
fn leader_contested_maps_to_uncle_fast_fail_reason() {
    let exit_reason = DispatchExitReason::LeaderContested;
    let stop_reason = exit_reason.to_stop_reason();

    assert_eq!(stop_reason, CoordinatorStopReason::LeaderContested);
    assert!(
        !stop_reason.is_clean(),
        "Contested lease is not a clean expected exit."
    );
}

#[test]
fn coordinator_shutdown_events_round_trip_in_db() {
    let dir = tempdir().expect("tempdir");
    let db_path =
        camino::Utf8PathBuf::from_path_buf(dir.path().join("grove.db")).expect("utf8 db path");
    let mut db = Database::open(&db_path).expect("open db");
    db.migrate().expect("migrate");

    let now = chrono::Utc::now();
    db.write_event_log(
        EventKind::CoordinatorStopped,
        None,
        None,
        None,
        &serde_json::json!({
            "stop_reason": CoordinatorStopReason::Interrupted.as_str(),
            "forced_termination": true,
            "running_session_count": 1,
            "leader_released": true,
        }),
        &now,
    )
    .expect("write coordinator stop event");

    let row: (String, String) = db
        .connection()
        .query_row(
            "SELECT kind, payload_json FROM event_log ORDER BY id DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read event row");

    assert_eq!(row.0, "CoordinatorStopped");
    let payload: serde_json::Value = serde_json::from_str(&row.1).expect("payload json");
    assert_eq!(payload["stop_reason"], "interrupted");
    assert_eq!(payload["forced_termination"], true);
    assert_eq!(payload["leader_released"], true);
}

#[test]
fn live_session_hooks_expose_idle_blocked_and_ready_activity_transitions() -> TestResult {
    let dir = tempdir()?;
    let workspace_dir = dir.path().join("workspace");
    fs::create_dir_all(&workspace_dir)?;
    let workspace_dir = Utf8PathBuf::from_path_buf(workspace_dir)
        .map_err(|_| io::Error::other("workspace dir must be valid UTF-8"))?;

    let script_path = dir.path().join("fake-claude");
    write_fake_claude_script(&script_path)?;
    let backend = CliClaudeBackend::new(script_path.to_string_lossy().into_owned());

    let mut request = sample_request(workspace_dir);
    request.idle_grace_period = Duration::from_millis(20);
    request.env = vec![
        ("PRE_OUTPUT_SLEEP_SECS".to_owned(), "0.05".to_owned()),
        (
            "STDOUT_SCRIPT".to_owned(),
            "working through the task\nGROVE_CHECKPOINT: {\"progress\":\"halfway\",\"next_step\":\"finish proof\",\"context\":{},\"open_questions\":[],\"claimed_paths\":[]}\n".to_owned(),
        ),
        (
            "STDERR_SCRIPT".to_owned(),
            "permission denied opening sandboxed path\n".to_owned(),
        ),
        ("EXIT_CODE".to_owned(), "0".to_owned()),
    ];

    let mut hooks = ActivityRecordingHooks::default();
    let _result = execute_single_task_session_with_hooks(&backend, request, &mut hooks)?;

    let started = hooks.started.lock().expect("lock started");
    assert_eq!(started.len(), 1, "session start should still be observable");
    drop(started);

    let changes = hooks.changes.lock().expect("lock activity changes");
    let activities: Vec<_> = changes.iter().map(|(activity, _, _)| *activity).collect();
    let details: Vec<_> = changes
        .iter()
        .map(|(_, detail, _)| detail.as_deref())
        .collect();

    assert!(
        activities.contains(&AgentActivity::Ready),
        "checkpoint output should mark the session ready"
    );
    assert!(
        activities.contains(&AgentActivity::Blocked),
        "permission denied stderr should mark the session blocked"
    );
    assert!(
        activities.contains(&AgentActivity::Idle),
        "stream timeout should eventually mark the session idle"
    );
    assert!(
        details.contains(&Some("protocol_event")),
        "checkpoint-driven activity change should be tagged as a protocol event"
    );
    assert!(
        details.contains(&Some("stderr")),
        "stderr-driven activity change should be tagged as stderr"
    );
    assert!(
        details.contains(&Some("stream_timeout")),
        "idle detection should be tagged as stream_timeout"
    );
    Ok(())
}

#[test]
fn slow_starting_session_does_not_flip_idle_before_first_output() -> TestResult {
    let dir = tempdir()?;
    let workspace_dir = dir.path().join("workspace");
    fs::create_dir_all(&workspace_dir)?;
    let workspace_dir = Utf8PathBuf::from_path_buf(workspace_dir)
        .map_err(|_| io::Error::other("workspace dir must be valid UTF-8"))?;

    let script_path = dir.path().join("fake-claude");
    write_fake_claude_script(&script_path)?;
    let backend = CliClaudeBackend::new(script_path.to_string_lossy().into_owned());

    let mut request = sample_request(workspace_dir);
    request.env = vec![
        ("PRE_OUTPUT_SLEEP_SECS".to_owned(), "0.05".to_owned()),
        (
            "STDOUT_SCRIPT".to_owned(),
            "working through the task\nGROVE_EXIT: true\nall tasks complete\nimplementation complete\n"
                .to_owned(),
        ),
        ("STDERR_SCRIPT".to_owned(), String::new()),
        ("EXIT_CODE".to_owned(), "0".to_owned()),
    ];

    let mut hooks = ActivityRecordingHooks::default();
    let _result = execute_single_task_session_with_hooks(&backend, request, &mut hooks)?;

    let changes = hooks.changes.lock().expect("lock activity changes");
    assert!(
        !changes
            .iter()
            .any(|(activity, _, _)| *activity == AgentActivity::Idle),
        "default idle grace period should not mark a session idle before its first delayed output"
    );
    Ok(())
}

#[test]
fn persisted_session_records_live_idle_transition_in_run_state_and_event_log() -> TestResult {
    let dir = tempdir()?;
    let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
        .map_err(|_| io::Error::other("db path must be valid UTF-8"))?;
    let mut db = Database::open(&db_path)?;
    db.migrate()?;
    db.connection().execute(
        "INSERT INTO bead_cache(\
            bead_id, title, description, priority, issue_type, status, assignee,\
            labels_json, parent_ids_json, dependency_ids_json, dependent_ids_json, raw_json, synced_at\
         ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, NULL, '[]', '[]', '[]', '[]', ?6, ?7)",
        rusqlite::params![
            "grove-244",
            "Prove live activity transitions",
            1,
            "task",
            "open",
            serde_json::json!({"id": "grove-244", "title": "Prove live activity transitions"}).to_string(),
            "2026-03-21T00:00:00Z"
        ],
    )?;

    let workspace_dir = dir.path().join("workspace");
    fs::create_dir_all(&workspace_dir)?;
    let workspace_dir = Utf8PathBuf::from_path_buf(workspace_dir)
        .map_err(|_| io::Error::other("workspace dir must be valid UTF-8"))?;
    init_trace_logging(&workspace_dir, true)?;

    let script_path = dir.path().join("fake-claude");
    write_fake_claude_script(&script_path)?;
    let backend = CliClaudeBackend::new(script_path.to_string_lossy().into_owned());

    let mut request = sample_request(workspace_dir.clone());
    request.idle_grace_period = Duration::from_millis(20);
    request.env = vec![
        ("PRE_OUTPUT_SLEEP_SECS".to_owned(), "0.05".to_owned()),
        (
            "STDOUT_SCRIPT".to_owned(),
            "working through the task\n".to_owned(),
        ),
        (
            "STDERR_SCRIPT".to_owned(),
            "permission denied opening sandboxed path\n".to_owned(),
        ),
        ("EXIT_CODE".to_owned(), "0".to_owned()),
    ];

    let _persisted = execute_persisted_single_task_session(
        &mut db,
        &backend,
        request,
        1,
        &GroveConfig::default(),
    )?;

    let run_rows = db.list_task_runs_for_bead(&BeadId::new("grove-244"))?;
    assert_eq!(run_rows.len(), 1, "expected one persisted run");
    assert!(
        run_rows[0].last_activity_at.is_some(),
        "activity persistence should stamp last_activity_at"
    );

    let events = db.list_event_logs_for_bead(&BeadId::new("grove-244"))?;
    let idle_event = events.iter().find(|event| {
        event.kind == EventKind::ActivityStateChanged
            && event.payload["activity"] == "Idle"
            && event.payload.get("detail")
                == Some(&serde_json::Value::String("stream_timeout".to_owned()))
    });
    assert!(
        idle_event.is_some(),
        "expected a durable Idle activity event tagged stream_timeout"
    );

    let blocked_event = events.iter().find(|event| {
        event.kind == EventKind::ActivityStateChanged
            && event.payload["activity"] == "Blocked"
            && event.payload.get("detail") == Some(&serde_json::Value::String("stderr".to_owned()))
    });
    assert!(
        blocked_event.is_some(),
        "expected a durable Blocked activity event tagged stderr"
    );

    let trace_path = workspace_dir
        .join(".grove")
        .join("logs")
        .join("runtime.jsonl");
    let trace = fs::read_to_string(trace_path.as_std_path())?;
    assert!(trace.contains("session.activity_changed"));
    assert!(trace.contains("session.finished"));
    Ok(())
}

#[test]
fn run_metrics_aggregation_includes_checkpoints_and_events() -> TestResult {
    let dir = tempdir()?;
    let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
        .map_err(|_| io::Error::other("db path must be valid UTF-8"))?;
    let mut db = Database::open(&db_path)?;
    db.migrate()?;

    db.connection().execute(
        "INSERT INTO bead_cache(\
            bead_id, title, description, priority, issue_type, status, assignee,\
            labels_json, parent_ids_json, dependency_ids_json, dependent_ids_json, raw_json, synced_at\
         ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, NULL, '[]', '[]', '[]', '[]', ?6, ?7)",
        rusqlite::params![
            "grove-metrics-test",
            "Test metrics aggregation",
            1,
            "task",
            "open",
            serde_json::json!({"id": "grove-metrics-test"}).to_string(),
            "2026-03-21T00:00:00Z"
        ],
    )?;

    let started = db.record_run_started(RunStartInput {
        run_id: RunId::new("run-metrics-test"),
        bead_id: BeadId::new("grove-metrics-test"),
        attempt_no: 1,
        started_at: "2026-03-21T10:00:00Z".parse()?,
        escalation_tier: EscalationTier::FirstAttempt,
    })?;
    assert_eq!(started.activity, Some(AgentActivity::Active));

    db.record_run_finished(
        &BeadId::new("grove-metrics-test"),
        RunFinishInput {
            run_id: RunId::new("run-metrics-test"),
            status: RunStatus::Succeeded,
            failure_class: None,
            failure_detail: None,
            ended_at: "2026-03-21T10:15:00Z".parse()?,
            retry_after: None,
            circuit_breaker_state: None,
        },
    )?;

    let metrics = db.aggregate_run_metrics(&RunId::new("run-metrics-test"))?;
    assert!(metrics.is_some(), "expected metrics for completed run");
    let metrics = metrics.unwrap();

    assert_eq!(metrics.run_id.as_str(), "run-metrics-test");
    assert_eq!(
        metrics.checkpoints_taken, 0,
        "checkpoint_count from run record"
    );

    let report = db.generate_run_report(&RunId::new("run-metrics-test"))?;
    assert!(report.is_some(), "expected run report");
    let report = report.unwrap();

    assert_eq!(report.bead_id.as_str(), "grove-metrics-test");
    assert_eq!(report.status, RunStatus::Succeeded);
    assert!(
        report.event_count > 0,
        "should have events from run lifecycle"
    );

    Ok(())
}

#[test]
fn failed_run_report_includes_failure_class_and_duration() -> TestResult {
    let dir = tempdir()?;
    let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
        .map_err(|_| io::Error::other("db path must be valid UTF-8"))?;
    let mut db = Database::open(&db_path)?;
    db.migrate()?;

    db.connection().execute(
        "INSERT INTO bead_cache(\
            bead_id, title, description, priority, issue_type, status, assignee,\
            labels_json, parent_ids_json, dependency_ids_json, dependent_ids_json, raw_json, synced_at\
         ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, NULL, '[]', '[]', '[]', '[]', ?6, ?7)",
        rusqlite::params![
            "grove-failure-test",
            "Test failure reporting",
            1,
            "task",
            "open",
            serde_json::json!({"id": "grove-failure-test"}).to_string(),
            "2026-03-21T00:00:00Z"
        ],
    )?;

    db.record_run_started(RunStartInput {
        run_id: RunId::new("run-failure-test"),
        bead_id: BeadId::new("grove-failure-test"),
        attempt_no: 1,
        started_at: "2026-03-21T10:00:00Z".parse()?,
        escalation_tier: EscalationTier::FirstAttempt,
    })?;

    db.record_run_finished(
        &BeadId::new("grove-failure-test"),
        RunFinishInput {
            run_id: RunId::new("run-failure-test"),
            status: RunStatus::Failed,
            failure_class: Some(FailureClass::Timeout),
            failure_detail: Some("Session exceeded maximum timeout".to_owned()),
            ended_at: "2026-03-21T10:05:00Z".parse()?,
            retry_after: None,
            circuit_breaker_state: None,
        },
    )?;

    let report = db.generate_run_report(&RunId::new("run-failure-test"))?;
    assert!(report.is_some(), "expected report for failed run");
    let report = report.unwrap();

    assert_eq!(report.status, RunStatus::Failed);
    assert_eq!(
        report.failure_class,
        Some(FailureClass::Timeout),
        "failure class should be captured in report"
    );
    assert!(
        report.metrics.total_duration_secs > 0,
        "should have duration"
    );

    Ok(())
}

#[test]
fn interrupted_run_reconciliation_marks_active_runs_retryable() -> TestResult {
    let dir = tempdir()?;
    let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
        .map_err(|_| io::Error::other("db path must be valid UTF-8"))?;
    let mut db = Database::open(&db_path)?;
    db.migrate()?;

    db.connection().execute(
        "INSERT INTO bead_cache(\
            bead_id, title, description, priority, issue_type, status, assignee,\
            labels_json, parent_ids_json, dependency_ids_json, dependent_ids_json, raw_json, synced_at\
         ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, NULL, '[]', '[]', '[]', '[]', ?6, ?7)",
        rusqlite::params![
            "grove-interrupt-test",
            "Test interrupted run",
            1,
            "task",
            "open",
            serde_json::json!({"id": "grove-interrupt-test"}).to_string(),
            "2026-03-21T00:00:00Z"
        ],
    )?;

    let start_time = "2026-03-21T10:00:00Z".parse::<chrono::DateTime<Utc>>()?;
    let now = "2026-03-21T10:30:00Z".parse::<chrono::DateTime<Utc>>()?;

    db.connection().execute(
        "INSERT INTO task_runs(\
            id, bead_id, attempt_no, status, failure_class, failure_detail, started_at, ended_at, session_count, checkpoint_count, last_checkpoint_id, activity, last_activity_at, escalation_tier\
         ) VALUES (?1, ?2, ?3, ?4, NULL, NULL, ?5, NULL, ?6, ?7, NULL, ?8, ?9, ?10)",
        rusqlite::params![
            "run-interrupt-test",
            "grove-interrupt-test",
            1,
            "Active",
            start_time.to_rfc3339(),
            1,
            0,
            "Active",
            start_time.to_rfc3339(),
            "FirstAttempt"
        ],
    )?;

    db.connection().execute(
        "INSERT INTO bead_runtime(\
            bead_id, grove_status, declared_paths_json, metadata_json, last_run_id, retry_after,\
            last_failure_class, last_failure_detail, runtime_updated_at\
         ) VALUES (?1, ?2, '[]', '{}', ?3, NULL, NULL, NULL, ?4)",
        rusqlite::params![
            "grove-interrupt-test",
            "Running",
            "run-interrupt-test",
            now.to_rfc3339()
        ],
    )?;

    let recovered = db.reconcile_interrupted_runs(&now)?;
    assert_eq!(recovered.len(), 1, "should recover one interrupted run");
    assert_eq!(
        recovered[0].run.status,
        RunStatus::WaitingToRetry,
        "interrupted run without checkpoint should be marked retryable"
    );
    assert_eq!(
        recovered[0].run.failure_class,
        Some(FailureClass::Interrupted)
    );

    let events = db.list_events_for_run(&RunId::new("run-interrupt-test"))?;
    assert!(
        events
            .iter()
            .any(|e| e.kind == EventKind::RecoveryActionTaken),
        "recovery action should be logged"
    );

    Ok(())
}
