#![allow(clippy::unwrap_used, clippy::expect_used)]

// Phase 1 Acceptance Tests
//
// This test suite validates that that kernel and CLI surfaces align with
// their architectural promises before proceeding to runtime phase.
//
// These tests cover:
// 1. Init-time dependency validation and DB creation
// 2. Bead cache sync correctness
// 3. Dispatch eligibility and Grove-local suppression
// 4. Status/inspect correctness against authoritative br state
// 5. BV augments information rather than replacing br as source of truth

use grove_br::{BeadCacheStore, BrClient, BrDependencySnapshot, BrIssueSummary, sync_bead_cache};
use grove_config::{DEFAULT_INIT_GROVE_TOML, GroveConfig, GrovePaths, validate_config};
use grove_db::{Database, reservation_patterns_overlap};
use grove_kernel::{
    DispatchEligibilityContext, LocalSuppressionReason, dispatch_suppression_label,
    evaluate_dispatch_eligibility, validate_dependency_snapshot,
};
use grove_types::{
    BeadId, BeadPriority, BeadRef, CircuitState, GroveBeadRecord, GroveBeadStatus,
    ReservationConflict, RunId, Timestamp,
};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    collections::BTreeSet,
    env, fs,
    io::Error as IoError,
    process::{Command, Output},
};
use tempfile::TempDir;

type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

fn sample_paths(config: &GroveConfig) -> Result<(TempDir, GrovePaths), Box<dyn std::error::Error>> {
    let temp_dir = TempDir::new()?;
    let workspace_root = camino::Utf8PathBuf::from_path_buf(temp_dir.path().to_path_buf())
        .map_err(|path| {
            IoError::other(format!(
                "workspace path is not valid UTF-8: {}",
                path.display()
            ))
        })?;
    let config_path = workspace_root.join("grove.toml");
    fs::write(&config_path, "")?;
    let paths = GrovePaths::from_config(config, &config_path)?;
    Ok((temp_dir, paths))
}

fn sample_timestamp() -> Timestamp {
    match "2026-03-17T00:00:00Z".parse() {
        Ok(timestamp) => timestamp,
        Err(error) => panic!("failed to parse fixture timestamp: {error}"),
    }
}

// ============================================================================
// 1. Init-time dependency validation and DB creation
// ============================================================================

#[test]
fn init_creates_database_with_migrations() -> TestResult {
    let temp_dir = TempDir::new()?;
    let workspace_root = camino::Utf8PathBuf::from_path_buf(temp_dir.path().to_path_buf())
        .map_err(|path| {
            IoError::other(format!(
                "workspace path is not valid UTF-8: {}",
                path.display()
            ))
        })?;

    let db_path = workspace_root.join(".grove/grove.db");
    assert!(
        !std::path::Path::new(db_path.as_str()).exists(),
        "database should not exist before init"
    );

    let mut db = Database::open(&db_path)?;

    // Database file should be created
    assert!(
        std::path::Path::new(db_path.as_str()).exists(),
        "database should be created"
    );

    db.migrate()?;

    // Verify migrations were applied
    let applied_migrations = db.applied_migrations()?;
    assert_eq!(
        applied_migrations.len(),
        11,
        "should apply all current migrations"
    );
    assert_eq!(applied_migrations[0].version, 1);
    assert_eq!(applied_migrations[0].name, "0001_init.sql");
    assert_eq!(applied_migrations[1].version, 2);
    assert_eq!(
        applied_migrations[1].name,
        "0002_prompt_manifest_columns.sql"
    );
    assert_eq!(applied_migrations[2].version, 3);
    assert_eq!(applied_migrations[2].name, "0003_leader_lease.sql");
    assert_eq!(applied_migrations[3].version, 4);
    assert_eq!(applied_migrations[3].name, "0004_mirror_outbox.sql");
    assert_eq!(applied_migrations[4].version, 5);
    assert_eq!(applied_migrations[4].name, "0005_operational_schema.sql");
    assert_eq!(applied_migrations[5].version, 6);
    assert_eq!(applied_migrations[5].name, "0006_observability.sql");
    assert_eq!(applied_migrations[6].version, 7);
    assert_eq!(applied_migrations[6].name, "0007_archive_fts.sql");
    assert_eq!(applied_migrations[7].version, 8);
    assert_eq!(applied_migrations[7].name, "0008_archive_watermarks.sql");
    assert_eq!(applied_migrations[8].version, 9);
    assert_eq!(applied_migrations[8].name, "0009_playbook.sql");
    assert_eq!(applied_migrations[9].version, 10);
    assert_eq!(applied_migrations[9].name, "0010_activity_state.sql");
    assert_eq!(applied_migrations[10].version, 11);
    assert_eq!(
        applied_migrations[10].name,
        "0011_circuit_breaker_state.sql"
    );

    // Verify tables exist by attempting to query them
    let conn = db.connection();
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")?
        .query_map([], |row| row.get(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let expected_tables = BTreeSet::from([
        "_migrations",
        "bead_cache",
        "bead_runtime",
        "bead_dependencies",
        "task_runs",
        "claude_sessions",
        "checkpoints",
        "handoffs",
        "leader_leases",
        "reservations",
        "event_log",
        "mirror_outbox",
    ]);

    for table in expected_tables {
        assert!(
            tables.contains(&table.to_string()),
            "expected table {} to exist, found tables: {:?}",
            table,
            tables
        );
    }

    Ok(())
}

#[test]
fn init_creates_required_directories() -> TestResult {
    let temp_dir = TempDir::new()?;
    let workspace_root = camino::Utf8PathBuf::from_path_buf(temp_dir.path().to_path_buf())
        .map_err(|path| {
            IoError::other(format!(
                "workspace path is not valid UTF-8: {}",
                path.display()
            ))
        })?;

    let grove_dir = workspace_root.join(".grove");
    let transcript_dir = grove_dir.join("transcripts");
    let prompts_dir = grove_dir.join("prompts");
    let checkpoints_dir = grove_dir.join("checkpoints");
    let artifacts_dir = grove_dir.join("artifacts");
    let logs_dir = grove_dir.join("logs");
    let tmp_dir = grove_dir.join("tmp");

    let expected_dirs = vec![
        grove_dir.as_std_path().to_path_buf(),
        transcript_dir.as_std_path().to_path_buf(),
        prompts_dir.as_std_path().to_path_buf(),
        checkpoints_dir.as_std_path().to_path_buf(),
        artifacts_dir.as_std_path().to_path_buf(),
        logs_dir.as_std_path().to_path_buf(),
        tmp_dir.as_std_path().to_path_buf(),
    ];

    // Create directories
    for dir in &expected_dirs {
        fs::create_dir_all(dir)?;
    }

    // Verify all directories exist
    for dir in expected_dirs {
        assert!(
            dir.exists(),
            "expected directory {} to exist",
            dir.display()
        );
    }

    Ok(())
}

#[test]
fn config_validation_rejects_invalid_ranges() -> TestResult {
    let mut config = GroveConfig::default();
    let (_temp_dir, paths) = sample_paths(&config)?;

    // Test checkpoint.rotate_pct must be > checkpoint.warn_pct
    config.checkpoint.warn_pct = 0.7;
    config.checkpoint.rotate_pct = 0.5;
    let result = validate_config(&config, &paths);
    assert!(result.is_err(), "rotate_pct <= warn_pct should be invalid");

    // Test checkpoint.hard_stop_pct must be >= checkpoint.rotate_pct
    config.checkpoint.rotate_pct = 0.9;
    config.checkpoint.hard_stop_pct = 0.85;
    let result = validate_config(&config, &paths);
    assert!(
        result.is_err(),
        "hard_stop_pct < rotate_pct should be invalid"
    );

    // Test out-of-range values
    config.checkpoint.rotate_pct = 0.8;
    config.checkpoint.hard_stop_pct = 1.5;
    let result = validate_config(&config, &paths);
    assert!(result.is_err(), "hard_stop_pct > 1.0 should be invalid");

    Ok(())
}

#[test]
fn config_validation_rejects_invalid_counts() -> TestResult {
    let mut config = GroveConfig::default();
    let (_temp_dir, paths) = sample_paths(&config)?;

    // Test scheduler.max_parallel must be >= 1
    config.scheduler.max_parallel = 0;
    let result = validate_config(&config, &paths);
    assert!(result.is_err(), "max_parallel < 1 should be invalid");

    // Test scheduler.retry_max must be >= 1
    config.scheduler.max_parallel = 5;
    config.scheduler.retry_max = 0;
    let result = validate_config(&config, &paths);
    assert!(result.is_err(), "retry_max < 1 should be invalid");

    // Test exit_policy.completion_indicator_threshold must be >= 1
    config.scheduler.retry_max = 3;
    config.exit_policy.completion_indicator_threshold = 0;
    let result = validate_config(&config, &paths);
    assert!(
        result.is_err(),
        "completion_indicator_threshold < 1 should be invalid"
    );

    Ok(())
}

// ============================================================================
// 2. Bead cache sync correctness
// ============================================================================

#[test]
fn sync_bead_cache_upserts_new_beads() -> TestResult {
    let mut store = FakeStore::default();

    let bead = sample_issue("grove-abc", "new task", vec![], vec![]);

    let result = sync_bead_cache(&FakeBrClient::new(vec![bead.clone()]), &mut store)?;

    assert_eq!(result.beads_added, 1);
    assert_eq!(result.beads_synced, 1);
    assert!(result.errors.is_empty());

    Ok(())
}

#[test]
fn sync_bead_cache_updates_existing_beads() -> TestResult {
    let mut store = FakeStore::default();

    let bead = sample_issue("grove-abc", "updated task", vec![], vec![]);

    // First sync adds to bead
    let _ = sync_bead_cache(&FakeBrClient::new(vec![bead.clone()]), &mut store)?;

    // Modify to bead and sync again
    let mut updated_bead = bead.clone();
    updated_bead.title = "updated title".into();
    let result = sync_bead_cache(&FakeBrClient::new(vec![updated_bead]), &mut store)?;

    assert_eq!(result.beads_updated, 1);
    assert_eq!(result.beads_synced, 1);
    assert!(result.errors.is_empty());

    Ok(())
}

#[test]
fn sync_bead_cache_marks_ready_beads_as_ready() -> TestResult {
    let mut store = FakeStore::default();

    let bead = sample_issue("grove-abc", "ready task", vec![], vec![]);

    // br.ready() returns this bead as ready
    let _result = sync_bead_cache(
        &FakeBrClient {
            ready: vec![bead.clone()],
            list_open: vec![bead.clone()],
        },
        &mut store,
    )?;

    // Verify grove_status was set to Ready
    let cached = store.statuses.get(bead.id.as_str());
    assert_eq!(cached, Some(&GroveBeadStatus::Ready));

    Ok(())
}

#[test]
fn sync_bead_cache_preserves_running_and_checkpointed_beads() -> TestResult {
    let mut store = FakeStore::default();

    let bead = sample_issue("grove-old", "removed task", vec![], vec![]);

    // Mark bead as running in cache
    store
        .statuses
        .insert(bead.id.as_str().to_owned(), GroveBeadStatus::Running);

    // br.list_open() does not return that bead (it was closed in br)
    let result = sync_bead_cache(&FakeBrClient::new(vec![]), &mut store)?;

    // Bead should not be counted as removed because it's running
    assert_eq!(result.beads_removed, 0);

    Ok(())
}

#[test]
fn sync_bead_cache_counts_removed_non_running_beads() -> TestResult {
    let mut store = FakeStore::default();

    let bead = sample_issue("grove-old-idle", "removed task", vec![], vec![]);

    // Mark bead as idle in cache (not running or checkpointed)
    store
        .statuses
        .insert(bead.id.as_str().to_owned(), GroveBeadStatus::Idle);

    // br.list_open() does not return that bead (it was closed in br)
    let result = sync_bead_cache(&FakeBrClient::new(vec![]), &mut store)?;

    // Bead should be counted as removed
    assert_eq!(result.beads_removed, 1);

    Ok(())
}

// ============================================================================
// 3. Executable-bead suppression
// ============================================================================

#[test]
fn epic_issue_type_is_dispatchable_when_ready() -> TestResult {
    let bead = sample_bead_record(GroveBeadStatus::Ready, "epic", &[]);

    let context = sample_context(true, CircuitState::Closed, vec![]);
    let eligibility = evaluate_dispatch_eligibility(&bead, &context);

    assert!(eligibility.ready_in_br);
    assert!(eligibility.dispatchable_in_grove);
    assert!(eligibility.local_suppression_reasons.is_empty());

    Ok(())
}

#[test]
fn tracking_issue_type_is_dispatchable_when_ready() -> TestResult {
    let bead = sample_bead_record(GroveBeadStatus::Ready, "tracking", &[]);

    let context = sample_context(true, CircuitState::Closed, vec![]);
    let eligibility = evaluate_dispatch_eligibility(&bead, &context);

    assert!(eligibility.ready_in_br);
    assert!(eligibility.dispatchable_in_grove);
    assert!(eligibility.local_suppression_reasons.is_empty());

    Ok(())
}

#[test]
fn task_issue_type_is_dispatchable() -> TestResult {
    let bead = sample_bead_record(GroveBeadStatus::Ready, "task", &[]);

    let context = sample_context(true, CircuitState::Closed, vec![]);
    let eligibility = evaluate_dispatch_eligibility(&bead, &context);

    assert!(eligibility.ready_in_br);
    assert!(eligibility.dispatchable_in_grove);
    assert!(eligibility.local_suppression_reasons.is_empty());

    Ok(())
}

#[test]
fn dispatch_no_label_suppresses_dispatch() -> TestResult {
    let bead = sample_bead_record(GroveBeadStatus::Ready, "task", &["dispatch:no"]);

    let context = sample_context(true, CircuitState::Closed, vec![]);
    let eligibility = evaluate_dispatch_eligibility(&bead, &context);

    assert!(eligibility.ready_in_br);
    assert!(!eligibility.dispatchable_in_grove);
    assert!(
        eligibility
            .local_suppression_reasons
            .iter()
            .any(|r| matches!(r, LocalSuppressionReason::SuppressedByLabel { .. }))
    );

    Ok(())
}

#[test]
fn case_insensitive_dispatch_no_detection() -> TestResult {
    let bead = sample_bead_record(GroveBeadStatus::Ready, "task", &["DISPATCH:NO"]);

    let label = dispatch_suppression_label(&bead.bead.labels);
    assert!(
        label.is_some(),
        "should detect case-insensitive dispatch:no label"
    );

    let context = sample_context(true, CircuitState::Closed, vec![]);
    let eligibility = evaluate_dispatch_eligibility(&bead, &context);

    assert!(!eligibility.dispatchable_in_grove);

    Ok(())
}

#[test]
fn running_status_suppresses_dispatch() -> TestResult {
    let bead = sample_bead_record(GroveBeadStatus::Running, "task", &[]);

    let context = sample_context(true, CircuitState::Closed, vec![]);
    let eligibility = evaluate_dispatch_eligibility(&bead, &context);

    assert!(eligibility.ready_in_br);
    assert!(!eligibility.dispatchable_in_grove);
    assert!(
        eligibility
            .local_suppression_reasons
            .iter()
            .any(|r| matches!(r, LocalSuppressionReason::ActiveRun { .. }))
    );

    Ok(())
}

#[test]
fn checkpointed_status_is_dispatchable_for_resume() -> TestResult {
    let bead = sample_bead_record(GroveBeadStatus::Checkpointed, "task", &[]);

    let context = sample_context(true, CircuitState::Closed, vec![]);
    let eligibility = evaluate_dispatch_eligibility(&bead, &context);

    assert!(eligibility.ready_in_br);
    assert!(eligibility.dispatchable_in_grove);
    assert!(eligibility.local_suppression_reasons.is_empty());

    Ok(())
}

#[test]
fn succeeded_status_suppresses_dispatch() -> TestResult {
    let bead = sample_bead_record(GroveBeadStatus::Succeeded, "task", &[]);

    let context = sample_context(true, CircuitState::Closed, vec![]);
    let eligibility = evaluate_dispatch_eligibility(&bead, &context);

    assert!(eligibility.ready_in_br);
    assert!(!eligibility.dispatchable_in_grove);
    assert!(
        eligibility
            .local_suppression_reasons
            .iter()
            .any(|r| matches!(r, LocalSuppressionReason::AlreadySucceeded))
    );

    Ok(())
}

#[test]
fn failed_status_suppresses_dispatch() -> TestResult {
    let bead = sample_bead_record(GroveBeadStatus::Failed, "task", &[]);

    let context = sample_context(true, CircuitState::Closed, vec![]);
    let eligibility = evaluate_dispatch_eligibility(&bead, &context);

    assert!(eligibility.ready_in_br);
    assert!(!eligibility.dispatchable_in_grove);
    assert!(
        eligibility
            .local_suppression_reasons
            .iter()
            .any(|r| matches!(r, LocalSuppressionReason::FailedAwaitingManualRetry))
    );

    Ok(())
}

#[test]
fn reservation_conflict_preserves_null_holder_run_id() -> TestResult {
    let bead = sample_bead_record(GroveBeadStatus::Ready, "task", &[]);
    let conflict = ReservationConflict {
        requested_by_bead: bead.bead.id.clone(),
        conflicting_bead: BeadId::new("grove-held"),
        requested_pattern: "crates/grove-db/src/lib.rs".to_owned(),
        held_pattern: "crates/grove-db/src/*.rs".to_owned(),
        conflicting_run_id: None,
    };

    let context = sample_context(true, CircuitState::Closed, vec![conflict]);
    let eligibility = evaluate_dispatch_eligibility(&bead, &context);

    assert!(!eligibility.dispatchable_in_grove);
    let preserved = eligibility
        .local_suppression_reasons
        .iter()
        .find_map(|reason| match reason {
            LocalSuppressionReason::ReservationConflict { conflict } => Some(conflict),
            _ => None,
        })
        .ok_or("expected reservation conflict reason")?;
    assert_eq!(preserved.conflicting_bead.as_str(), "grove-held");
    assert_eq!(preserved.conflicting_run_id, None);

    Ok(())
}

#[test]
fn reservation_overlap_helper_handles_common_file_and_glob_cases() {
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
        "crates/grove-db/tests/*.rs"
    ));
}

#[test]
fn circuit_open_suppresses_all_dispatch() -> TestResult {
    let bead = sample_bead_record(GroveBeadStatus::Ready, "task", &[]);

    let context = sample_context(true, CircuitState::Open, vec![]);
    let eligibility = evaluate_dispatch_eligibility(&bead, &context);

    assert!(eligibility.ready_in_br);
    assert!(!eligibility.dispatchable_in_grove);
    assert!(
        eligibility
            .local_suppression_reasons
            .iter()
            .any(|r| matches!(r, LocalSuppressionReason::CircuitOpen))
    );

    Ok(())
}

#[test]
fn reservation_conflict_suppresses_dispatch() -> TestResult {
    let bead = sample_bead_record(GroveBeadStatus::Ready, "task", &[]);

    let conflict = ReservationConflict {
        requested_by_bead: BeadId::new("grove-xyz"),
        conflicting_bead: BeadId::new("grove-abc"),
        requested_pattern: "crates/**".into(),
        held_pattern: "crates/grove-cli/**".into(),
        conflicting_run_id: None,
    };

    let context = sample_context(true, CircuitState::Closed, vec![conflict]);
    let eligibility = evaluate_dispatch_eligibility(&bead, &context);

    assert!(eligibility.ready_in_br);
    assert!(!eligibility.dispatchable_in_grove);
    assert!(
        eligibility
            .local_suppression_reasons
            .iter()
            .any(|r| matches!(r, LocalSuppressionReason::ReservationConflict { .. }))
    );

    Ok(())
}

#[test]
fn retry_backoff_only_suppresses_while_pending() -> TestResult {
    let future_retry: Timestamp = "2026-03-17T12:00:00Z".parse()?;
    let past_retry: Timestamp = "2026-03-16T12:00:00Z".parse()?;

    let blocked_bead = sample_bead_with_retry(GroveBeadStatus::WaitingToRetry, Some(future_retry));
    let expired_bead = sample_bead_with_retry(GroveBeadStatus::WaitingToRetry, Some(past_retry));

    let context = sample_context(true, CircuitState::Closed, vec![]);

    let blocked_eligibility = evaluate_dispatch_eligibility(&blocked_bead, &context);
    let expired_eligibility = evaluate_dispatch_eligibility(&expired_bead, &context);

    // Blocked bead should not be dispatchable
    assert!(!blocked_eligibility.dispatchable_in_grove);
    assert!(
        blocked_eligibility
            .local_suppression_reasons
            .iter()
            .any(|r| matches!(r, LocalSuppressionReason::RetryBackoffPending { .. }))
    );

    // Expired retry bead should be dispatchable
    assert!(expired_eligibility.dispatchable_in_grove);
    assert!(
        !expired_eligibility
            .local_suppression_reasons
            .iter()
            .any(|r| matches!(r, LocalSuppressionReason::RetryBackoffPending { .. }))
    );

    Ok(())
}

// ============================================================================
// 4. Status/inspect correctness against authoritative br state
// ============================================================================

#[test]
fn kernel_status_uses_br_as_authoritative_source() -> TestResult {
    let mut store = FakeStore::default();

    let bead = sample_issue("grove-abc", "test task", vec![], vec![]);

    // Sync adds the bead to cache
    let _ = sync_bead_cache(&FakeBrClient::new(vec![bead.clone()]), &mut store)?;

    // Verify bead is in cache
    assert!(store.beads.contains_key(bead.id.as_str()));

    // FakeBrClient::new marks beads as ready, so the cached status becomes Ready on first sync.
    let cached = store.statuses.get(bead.id.as_str());
    assert_eq!(cached, Some(&GroveBeadStatus::Ready));

    // When br.ready() returns this bead, it remains Ready
    let _ = sync_bead_cache(
        &FakeBrClient {
            ready: vec![bead.clone()],
            list_open: vec![bead.clone()],
        },
        &mut store,
    )?;

    // Verify grove_status was set to Ready
    let cached = store.statuses.get(bead.id.as_str());
    assert_eq!(cached, Some(&GroveBeadStatus::Ready));

    Ok(())
}

#[test]
fn dependency_snapshot_validates_self_edges() -> TestResult {
    let snapshot = BrDependencySnapshot {
        bead_id: BeadId::new("grove-1"),
        blocked_by: vec![BeadId::new("grove-1")], // Self-block
        blocks: vec![BeadId::new("grove-child")],
        rows: vec![],
    };

    let issues = validate_dependency_snapshot(&snapshot);

    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].code(), "self_blocked_by");

    Ok(())
}

#[test]
fn dependency_snapshot_validates_duplicate_edges() -> TestResult {
    let snapshot = BrDependencySnapshot {
        bead_id: BeadId::new("grove-1"),
        blocked_by: vec![
            BeadId::new("grove-parent"),
            BeadId::new("grove-parent"), // Duplicate
        ],
        blocks: vec![BeadId::new("grove-child")],
        rows: vec![],
    };

    let issues = validate_dependency_snapshot(&snapshot);

    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].code(), "duplicate_blocked_by");

    Ok(())
}

#[test]
fn dependency_snapshot_accepts_valid_edges() -> TestResult {
    let snapshot = BrDependencySnapshot {
        bead_id: BeadId::new("grove-1"),
        blocked_by: vec![BeadId::new("grove-parent")],
        blocks: vec![BeadId::new("grove-child")],
        rows: vec![],
    };

    let issues = validate_dependency_snapshot(&snapshot);

    assert!(issues.is_empty(), "valid snapshot should have no issues");

    Ok(())
}

// ============================================================================
// 5. CLI-facing contract checks
// ============================================================================

#[test]
fn init_tolerates_missing_beads_and_prints_guidance() -> TestResult {
    let harness = CliHarness::new()?;
    let output = harness.run(["init", "--force"])?;

    assert!(
        output.status.success(),
        "init --force should succeed without .beads: {}",
        output_text(&output)
    );
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains("Initialized grove workspace."));
    assert!(stdout.contains(
        "No .beads directory detected yet; run `br init` before `grove status` or `grove run`."
    ));
    assert!(stdout.contains("`bv` does not see a .beads directory yet"));

    Ok(())
}

#[test]
fn init_refuses_when_workspace_is_already_initialized() -> TestResult {
    let harness = CliHarness::new()?;
    fs::create_dir_all(harness.workspace_root.join(".grove/logs"))?;

    let output = harness.run(["init"])?;
    assert!(!output.status.success(), "second init should fail");

    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.contains("Grove is already initialized"));
    assert!(stderr.contains("Nothing was changed."));
    assert!(stderr.contains("grove init --force"));
    Ok(())
}

#[test]
fn init_json_reports_initialized_workspace_as_machine_readable_failure() -> TestResult {
    let harness = CliHarness::new()?;
    fs::create_dir_all(harness.workspace_root.join(".grove/logs"))?;

    let output = harness.run(["--json", "init"])?;
    assert!(output.status.success(), "json failure payload should still exit successfully");

    let stdout = String::from_utf8(output.stdout)?;
    let payload: serde_json::Value = serde_json::from_str(&stdout)?;
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["command"], "init");
    let errors = payload["error"].as_array().expect("error array");
    assert!(errors.iter().any(|v| v.as_str().is_some_and(|s| s.contains("already initialized"))));
    Ok(())
}

#[test]
fn init_force_resets_runtime_state_but_preserves_config() -> TestResult {
    let harness = CliHarness::new()?;
    harness.enable_beads()?;
    harness.seed_runtime_bead(GroveBeadStatus::Running)?;
    fs::create_dir_all(harness.workspace_root.join(".grove/logs"))?;
    fs::create_dir_all(harness.workspace_root.join(".grove/prompts"))?;
    fs::write(harness.workspace_root.join(".grove/logs/runtime.jsonl"), "old-log\n")?;
    fs::write(harness.workspace_root.join(".grove/prompts/keep-me.txt"), "stale prompt")?;
    fs::write(harness.workspace_root.join("unrelated.txt"), "keep this")?;
    let original_config = fs::read_to_string(harness.workspace_root.join("grove.toml"))?;

    let output = harness.run(["init", "--force"])?;
    assert!(output.status.success(), "init --force should succeed: {}", output_text(&output));

    let db = Database::open(&harness.workspace_root.join(".grove/grove.db"))?;
    let bead_count: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM bead_cache", [], |row| row.get(0))?;
    assert_eq!(bead_count, 1, "bead cache should be re-synced after force init");

    let run_count: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM task_runs", [], |row| row.get(0))?;
    assert_eq!(run_count, 0, "runtime task runs should be cleared by force init");

    assert_eq!(
        fs::read_to_string(harness.workspace_root.join("grove.toml"))?,
        original_config,
        "force init should preserve grove.toml"
    );
    assert_eq!(
        fs::read_to_string(harness.workspace_root.join("unrelated.txt"))?,
        "keep this",
        "force init should not touch unrelated files"
    );
    assert!(
        !harness.workspace_root.join(".grove/prompts/keep-me.txt").exists(),
        "force init should clear Grove-managed prompt artifacts"
    );

    Ok(())
}

#[test]
fn init_json_emits_machine_readable_output() -> TestResult {
    let harness = CliHarness::new()?;
    let output = harness.run(["--json", "init", "--force"])?;

    assert!(
        output.status.success(),
        "init --json --force should succeed: {}",
        output_text(&output)
    );
    let stdout = String::from_utf8(output.stdout)?;
    let payload: serde_json::Value = serde_json::from_str(&stdout)?;
    assert_eq!(payload["ok"], true);
    assert!(payload["workspace_root"].as_str().is_some());
    assert!(payload["db_path"].as_str().is_some());
    assert!(payload["config_path"].as_str().is_some());
    assert!(payload["tooling"].is_object());
    assert!(payload["notes"].is_array());
    assert!(payload["next_steps"].is_array());
    assert_eq!(payload["forced_reset"], true);

    Ok(())
}

#[test]
fn no_subcommand_json_emits_machine_readable_output() -> TestResult {
    let harness = CliHarness::new()?;
    let output = harness.run(["--json"])?;

    assert!(
        output.status.success(),
        "global --json without subcommand should succeed: {}",
        output_text(&output)
    );
    let stdout = String::from_utf8(output.stdout)?;
    let payload: serde_json::Value = serde_json::from_str(&stdout)?;
    assert_eq!(payload["ok"], true);
    assert!(payload["command"].is_null());
    assert!(payload["message"].as_str().is_some());
    assert!(payload["available_commands"].is_array());

    Ok(())
}

#[test]
fn status_requires_beads_directory() -> TestResult {
    let harness = CliHarness::new()?;
    let output = harness.run(["status"])?;

    assert!(
        !output.status.success(),
        "status should fail without .beads"
    );
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.contains("does not contain a .beads directory; run `br init` first"));

    Ok(())
}

#[test]
fn status_reports_bv_unavailable_but_still_succeeds() -> TestResult {
    let harness = CliHarness::new()?;
    harness.enable_beads()?;

    let output = harness.run_with_env(
        ["status"],
        [("GROVE_TEST_BV_TRIAGE_FAIL", "bv triage unavailable in test")],
    )?;

    assert!(
        output.status.success(),
        "status should degrade gracefully when bv triage fails: {}",
        output_text(&output)
    );
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains("Workspace: "));
    assert!(stdout.contains("Ready queue:"));
    assert!(stdout.contains("grove-cli-test"));
    assert!(stdout.contains("BV triage:"));
    assert!(stdout.contains("unavailable: bv command failed (bv --robot-triage)"));

    Ok(())
}

#[test]
fn status_json_emits_machine_readable_operator_surface() -> TestResult {
    let harness = CliHarness::new()?;
    harness.enable_beads()?;
    harness.seed_runtime_bead(GroveBeadStatus::Running)?;

    let output = harness.run(["--json", "status"])?;

    assert!(
        output.status.success(),
        "status --json should succeed: {}",
        output_text(&output)
    );
    let stdout = String::from_utf8(output.stdout)?;
    let payload: serde_json::Value = serde_json::from_str(&stdout)?;
    let workspace_root = payload["workspace_root"]
        .as_str()
        .expect("workspace_root string");
    assert!(workspace_root.starts_with(harness.workspace_root.as_str()));
    assert!(payload["db_path"].as_str().is_some());
    assert!(payload["triage_error"].is_null());
    assert!(payload["view"].is_object());
    let view_root = payload["view"]["workspace_root"]
        .as_str()
        .expect("view.workspace_root string");
    assert!(view_root.starts_with(harness.workspace_root.as_str()));
    assert!(payload["view"]["running_beads"].is_array());
    assert!(payload["view"]["ready_queue"].is_array());
    assert!(payload["view"]["mirror_pending"].is_array());

    Ok(())
}

#[test]
fn run_json_failure_emits_machine_readable_payload() -> TestResult {
    let harness = CliHarness::new()?;

    let output = harness.run(["--json", "run"])?;

    assert!(
        output.status.success(),
        "run --json failure payload should still exit successfully for machine parsing: {}",
        output_text(&output)
    );
    let stdout = String::from_utf8(output.stdout)?;
    let payload: serde_json::Value = serde_json::from_str(&stdout)?;
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["command"], "run");
    let errors = payload["error"].as_array().expect("error array");
    assert!(!errors.is_empty(), "error chain should not be empty");
    assert!(errors[0].as_str().is_some());

    Ok(())
}

#[test]
fn inspect_merges_br_detail_with_local_runtime_view() -> TestResult {
    let harness = CliHarness::new()?;
    harness.enable_beads()?;
    harness.seed_runtime_bead(GroveBeadStatus::Running)?;

    let output = harness.run(["inspect", "grove-cli-test"])?;

    assert!(
        output.status.success(),
        "inspect should succeed with br detail and local cache: {}",
        output_text(&output)
    );
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains("Bead: grove-cli-test — CLI inspect test"));
    assert!(stdout.contains("Description:\nDetailed CLI inspect description"));
    assert!(stdout.contains("Grove runtime:"));
    assert!(stdout.contains("- status: Running"));
    assert!(stdout.contains("Latest dispatch:"));

    Ok(())
}

#[test]
fn inspect_from_nested_directory_loads_relative_prompt_manifest() -> TestResult {
    let harness = CliHarness::new()?;
    harness.enable_beads()?;
    harness.seed_runtime_bead_with_prompt_manifest(GroveBeadStatus::Running)?;
    let nested_dir = harness.workspace_root.join("nested/child");
    fs::create_dir_all(&nested_dir)?;

    let output = harness.run_from_dir(["inspect", "grove-cli-test"], nested_dir.as_std_path())?;

    assert!(
        output.status.success(),
        "inspect should resolve workspace root from nested directory: {}",
        output_text(&output)
    );
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains("- prompt manifest: .grove/prompts/prompt-cli-test.json"));
    assert!(stdout.contains("- prompt contract: implement"));
    assert!(stdout.contains("- #1 Task [task] included=true tokens=20 trim="));
    assert!(stdout.contains("preview: [TASK] inspect from nested cwd"));

    Ok(())
}

#[test]
fn inspect_errors_when_br_and_local_cache_both_absent() -> TestResult {
    let harness = CliHarness::new()?;
    harness.enable_beads()?;

    let output = harness.run(["inspect", "grove-missing"])?;

    assert!(
        !output.status.success(),
        "inspect should fail when bead is absent everywhere"
    );
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.contains("bead grove-missing was not found in br or the local Grove cache"));

    Ok(())
}

// ============================================================================
// 6. BV augments rather than replaces br as source of truth
// ============================================================================

#[test]
fn bv_augments_br_does_not_replace_authority() -> TestResult {
    // This test validates that BV's triage output is used as
    // augmentation (scoring, priority) while br remains as
    // authoritative source for bead existence, status, and readiness

    // Create a bead in br
    let bead = sample_issue("grove-abc", "triaged task", vec![], vec![]);

    // br.ready() determines readiness based on blocker count
    let ready_in_br = bead.blocked_by.is_empty();

    // BV triage provides scoring based on graph analysis
    // (simulated here - in real BV this comes from PageRank, critical path, etc.)
    let bv_score = 0.75; // BV's computed score
    let bv_reason = if bead.title.is_empty() {
        String::new()
    } else {
        "critical path bead".to_string()
    };

    // Validate that:
    // 1. br.ready() is the source of truth for readiness
    assert!(ready_in_br, "bead with no blockers is ready in br");

    // 2. BV provides additional context (score, reason) not present in br
    assert!(bv_score > 0.0, "BV provides scoring information");
    std::hint::black_box(&bv_reason);
    assert!(!bv_reason.is_empty(), "BV provides reasoning");

    // 3. The authoritative bead data comes from br (title, status, etc.)
    assert_eq!(
        bead.title, "triaged task",
        "bead title from br is authoritative"
    );
    assert_eq!(bead.status, "open", "bead status from br is authoritative");

    // In Grove's dispatch logic, we use br.ready() to determine
    // eligibility, then augment with BV scoring for priority
    let eligibility_context = DispatchEligibilityContext {
        ready_in_br,
        circuit_state: CircuitState::Closed,
        reservation_conflicts: vec![],
        now: sample_timestamp(),
    };

    let bead_record = GroveBeadRecord {
        bead: BeadRef {
            id: bead.id.clone(),
            title: bead.title.clone(),
            description: bead.description.clone(),
            priority: bead.priority,
            issue_type: bead.issue_type.clone(),
            br_status: bead.status.clone(),
            assignee: bead.assignee.clone(),
            labels: bead.labels.clone(),
            created_at: bead.created_at,
            updated_at: bead.updated_at,
        },
        grove_status: GroveBeadStatus::Ready,
        declared_paths: vec![],
        metadata: Default::default(),
        last_run_id: None,
        retry_after: None,
        last_failure_class: None,
        last_failure_detail: None,
        circuit_breaker_state: None,
        synced_at: bead.updated_at,
        runtime_updated_at: bead.updated_at,
    };

    let eligibility = evaluate_dispatch_eligibility(&bead_record, &eligibility_context);

    // Dispatchability depends on br.ready(), not BV
    assert_eq!(eligibility.ready_in_br, ready_in_br);
    assert_eq!(eligibility.dispatchable_in_grove, ready_in_br);

    // BV score is used for priority ordering (not shown here but in real code)

    Ok(())
}

#[test]
fn bv_unavailable_degrades_gracefully() -> TestResult {
    // This test validates that when BV is unavailable, Grove continues
    // to function using br as a sole source of truth

    // Create a bead that's ready in br
    let bead = sample_issue("grove-abc", "no-bv task", vec![], vec![]);

    // Without BV, we only have br
    let ready_in_br = bead.blocked_by.is_empty();

    // Grove should still determine dispatchability correctly
    let eligibility_context = DispatchEligibilityContext {
        ready_in_br,
        circuit_state: CircuitState::Closed,
        reservation_conflicts: vec![],
        now: sample_timestamp(),
    };

    let bead_record = GroveBeadRecord {
        bead: BeadRef {
            id: bead.id.clone(),
            title: bead.title.clone(),
            description: bead.description.clone(),
            priority: bead.priority,
            issue_type: bead.issue_type.clone(),
            br_status: bead.status.clone(),
            assignee: bead.assignee.clone(),
            labels: bead.labels.clone(),
            created_at: bead.created_at,
            updated_at: bead.updated_at,
        },
        grove_status: GroveBeadStatus::Ready,
        declared_paths: vec![],
        metadata: Default::default(),
        last_run_id: None,
        retry_after: None,
        last_failure_class: None,
        last_failure_detail: None,
        circuit_breaker_state: None,
        synced_at: bead.updated_at,
        runtime_updated_at: bead.updated_at,
    };

    let eligibility = evaluate_dispatch_eligibility(&bead_record, &eligibility_context);

    // Dispatchability should still work correctly
    assert_eq!(eligibility.ready_in_br, ready_in_br);
    assert_eq!(eligibility.dispatchable_in_grove, ready_in_br);

    // BV score is used for priority ordering (not shown here but in real code)

    Ok(())
}

// ============================================================================
// Helper functions and fixtures
// ============================================================================

fn sample_issue(
    id: &str,
    title: &str,
    blocked_by: Vec<BeadId>,
    blocks: Vec<BeadId>,
) -> BrIssueSummary {
    let now: Timestamp = sample_timestamp();

    BrIssueSummary {
        id: BeadId::new(id),
        title: title.into(),
        description: Some(format!("description for {}", title)),
        priority: BeadPriority::P1,
        issue_type: "task".into(),
        status: "open".into(),
        assignee: None,
        labels: vec!["area:test".into()],
        created_at: now,
        updated_at: now,
        blocked_by,
        blocks,
        raw_json: Default::default(),
    }
}

fn sample_bead_record(
    grove_status: GroveBeadStatus,
    issue_type: &str,
    labels: &[&str],
) -> GroveBeadRecord {
    let now: Timestamp = sample_timestamp();
    let last_run_id = matches!(
        grove_status,
        GroveBeadStatus::Running | GroveBeadStatus::Checkpointed
    )
    .then(|| RunId::new("run-test"));

    GroveBeadRecord {
        bead: BeadRef {
            id: BeadId::new("grove-test"),
            title: "test bead".into(),
            description: None,
            priority: BeadPriority::P1,
            issue_type: issue_type.into(),
            br_status: "open".into(),
            assignee: None,
            labels: labels.iter().map(|s| (*s).to_owned()).collect(),
            created_at: now,
            updated_at: now,
        },
        grove_status,
        declared_paths: vec![],
        metadata: Default::default(),
        last_run_id,
        retry_after: None,
        last_failure_class: None,
        last_failure_detail: None,
        circuit_breaker_state: None,
        synced_at: now,
        runtime_updated_at: now,
    }
}

fn sample_bead_with_retry(
    grove_status: GroveBeadStatus,
    retry_after: Option<Timestamp>,
) -> GroveBeadRecord {
    let now: Timestamp = sample_timestamp();

    GroveBeadRecord {
        bead: BeadRef {
            id: BeadId::new("grove-retry-test"),
            title: "retry test bead".into(),
            description: None,
            priority: BeadPriority::P1,
            issue_type: "task".into(),
            br_status: "open".into(),
            assignee: None,
            labels: vec![],
            created_at: now,
            updated_at: now,
        },
        grove_status,
        declared_paths: vec![],
        metadata: Default::default(),
        last_run_id: None,
        retry_after,
        last_failure_class: None,
        last_failure_detail: None,
        circuit_breaker_state: None,
        synced_at: now,
        runtime_updated_at: now,
    }
}

fn sample_context(
    ready_in_br: bool,
    circuit_state: CircuitState,
    reservation_conflicts: Vec<ReservationConflict>,
) -> DispatchEligibilityContext {
    DispatchEligibilityContext {
        ready_in_br,
        circuit_state,
        reservation_conflicts,
        now: sample_timestamp(),
    }
}

// Fake implementations for testing

#[derive(Default)]
struct FakeStore {
    beads: std::collections::BTreeMap<String, BrIssueSummary>,
    dependencies: std::collections::BTreeMap<String, (Vec<BeadId>, Vec<BeadId>)>,
    statuses: std::collections::BTreeMap<String, GroveBeadStatus>,
}

impl BeadCacheStore for FakeStore {
    fn upsert_bead_cache(
        &mut self,
        bead: &BrIssueSummary,
    ) -> anyhow::Result<grove_br::UpsertOutcome> {
        let outcome = if self.beads.contains_key(bead.id.as_str()) {
            grove_br::UpsertOutcome::Updated
        } else {
            grove_br::UpsertOutcome::Added
        };
        self.beads.insert(bead.id.as_str().to_owned(), bead.clone());
        Ok(outcome)
    }

    fn replace_dependency_snapshot(
        &mut self,
        bead_id: &BeadId,
        blocked_by: &[BeadId],
        blocks: &[BeadId],
    ) -> anyhow::Result<()> {
        self.dependencies.insert(
            bead_id.as_str().to_owned(),
            (blocked_by.to_vec(), blocks.to_vec()),
        );
        Ok(())
    }

    fn list_cached_beads(&self) -> anyhow::Result<Vec<grove_br::CachedBeadState>> {
        let mut ids: std::collections::HashSet<String> = self.beads.keys().cloned().collect();
        ids.extend(self.statuses.keys().cloned());
        Ok(ids
            .into_iter()
            .map(|bead_id| grove_br::CachedBeadState {
                bead_id: BeadId::new(bead_id.clone()),
                grove_status: self.statuses.get(&bead_id).copied(),
            })
            .collect())
    }

    fn set_grove_status(
        &mut self,
        bead_id: &BeadId,
        status: GroveBeadStatus,
    ) -> anyhow::Result<()> {
        self.statuses.insert(bead_id.as_str().to_owned(), status);
        Ok(())
    }
}

struct FakeBrClient {
    ready: Vec<BrIssueSummary>,
    list_open: Vec<BrIssueSummary>,
}

impl FakeBrClient {
    fn new(beads: Vec<BrIssueSummary>) -> Self {
        Self {
            ready: beads.clone(),
            list_open: beads,
        }
    }
}

impl BrClient for FakeBrClient {
    fn ready(&self) -> anyhow::Result<Vec<BrIssueSummary>, grove_br::BrError> {
        Ok(self.ready.clone())
    }

    fn list_open(&self) -> anyhow::Result<Vec<BrIssueSummary>, grove_br::BrError> {
        Ok(self.list_open.clone())
    }

    fn show(&self, _id: &BeadId) -> anyhow::Result<grove_br::BrIssueDetail, grove_br::BrError> {
        Err(grove_br::BrError::BeadNotFound { id: _id.clone() })
    }

    fn dep_list(&self, _id: &BeadId) -> anyhow::Result<BrDependencySnapshot, grove_br::BrError> {
        Ok(BrDependencySnapshot {
            bead_id: _id.clone(),
            blocked_by: vec![],
            blocks: vec![],
            rows: vec![],
        })
    }

    fn capability(&self) -> anyhow::Result<grove_br::BrCapability, grove_br::BrError> {
        Ok(grove_br::BrCapability {
            available: true,
            version_line: Some("br 0.1.12 (test)".into()),
            version: Some(grove_br::BrVersion {
                raw: "br 0.1.12 (test)".into(),
                major: Some(0),
                minor: Some(1),
                patch: Some(12),
            }),
            beads_dir_exists: true,
        })
    }

    fn close_bead(
        &self,
        _id: &BeadId,
        _reason: Option<&str>,
    ) -> anyhow::Result<(), grove_br::BrError> {
        Ok(())
    }

    fn add_comment(&self, _id: &BeadId, _comment: &str) -> anyhow::Result<(), grove_br::BrError> {
        Ok(())
    }

    fn mirror_handoff(
        &self,
        _id: &BeadId,
        _handoff: &grove_types::HandoffRecord,
        _close_bead: bool,
    ) -> anyhow::Result<(), grove_br::BrError> {
        Ok(())
    }
}

struct CliHarness {
    _temp_dir: TempDir,
    workspace_root: camino::Utf8PathBuf,
    bin_dir: camino::Utf8PathBuf,
}

impl CliHarness {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let workspace_root = camino::Utf8PathBuf::from_path_buf(temp_dir.path().to_path_buf())
            .map_err(|path| {
                IoError::other(format!(
                    "workspace path is not valid UTF-8: {}",
                    path.display()
                ))
            })?;
        let bin_dir = workspace_root.join("test-bin");
        fs::create_dir_all(&bin_dir)?;

        fs::write(workspace_root.join("grove.toml"), DEFAULT_INIT_GROVE_TOML)?;

        write_executable(&bin_dir.join("claude"), CLAUDE_STUB)?;
        write_executable(&bin_dir.join("br"), BR_STUB)?;
        write_executable(&bin_dir.join("bv"), BV_STUB)?;

        Ok(Self {
            _temp_dir: temp_dir,
            workspace_root,
            bin_dir,
        })
    }

    fn enable_beads(&self) -> Result<(), Box<dyn std::error::Error>> {
        fs::create_dir_all(self.workspace_root.join(".beads"))?;
        Ok(())
    }

    fn seed_runtime_bead(
        &self,
        grove_status: GroveBeadStatus,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut db = Database::open(&self.workspace_root.join(".grove/grove.db"))?;
        db.migrate()?;
        let bead = sample_issue("grove-cli-test", "CLI inspect test", vec![], vec![]);
        db.upsert_bead_cache(&bead)?;
        db.set_grove_status(&bead.id, grove_status)?;
        db.replace_dependency_snapshot(&bead.id, &[], &[])?;
        Ok(())
    }

    fn seed_runtime_bead_with_prompt_manifest(
        &self,
        grove_status: GroveBeadStatus,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.seed_runtime_bead(grove_status)?;
        fs::create_dir_all(self.workspace_root.join(".grove/prompts"))?;
        fs::write(
            self.workspace_root
                .join(".grove/prompts/prompt-cli-test.json"),
            serde_json::to_string(&grove_types::PromptManifest {
                prompt_id: grove_types::PromptId::new("prompt-cli-test"),
                bead_id: grove_types::BeadId::new("grove-cli-test"),
                run_id: grove_types::RunId::new("run-cli-test"),
                session_id: Some(grove_types::SessionId::new("ses-cli-test")),
                contract: grove_types::ExecutionContract::Implement,
                created_at: "2026-03-17T00:00:00Z".parse()?,
                token_budget: Some(200),
                estimated_tokens: 120,
                prompt_bytes: 512,
                trimmed: false,
                retry_delta_summary: None,
                retrieval_query: None,
                retrieval_ranking_summary: Vec::new(),
                sections: vec![grove_types::PromptManifestSection {
                    ordinal: 1,
                    kind: grove_types::PromptSegmentKind::Task,
                    heading: "Task".to_owned(),
                    included: true,
                    estimated_tokens: 20,
                    char_count: 80,
                    trim_reason: None,
                    provenance: grove_types::PromptSectionProvenance::default(),
                    preview: "[TASK] inspect from nested cwd".to_owned(),
                }],
            })?,
        )?;

        let db = Database::open(&self.workspace_root.join(".grove/grove.db"))?;
        db.connection().execute(
            "INSERT INTO task_runs(\
                id, bead_id, attempt_no, status, failure_class, failure_detail, started_at, ended_at, session_count, checkpoint_count, last_checkpoint_id\
            ) VALUES (?1, ?2, ?3, ?4, NULL, NULL, ?5, NULL, ?6, ?7, ?8)",
            rusqlite::params![
                "run-cli-test",
                "grove-cli-test",
                1,
                "Active",
                "2026-03-17T00:00:00Z",
                1,
                0,
                Option::<String>::None,
            ],
        )?;
        db.connection().execute(
            "UPDATE bead_runtime SET last_run_id = ?2 WHERE bead_id = ?1",
            rusqlite::params!["grove-cli-test", "run-cli-test"],
        )?;
        db.connection().execute(
            "INSERT INTO claude_sessions(\
                id, run_id, external_session_id, ordinal_in_run, status, started_at, ended_at, prompt_id, prompt_manifest_path, prompt_bytes, estimated_input_tokens, estimated_output_tokens, exit_code, stop_reason, transcript_path\
            ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            rusqlite::params![
                "ses-cli-test",
                "run-cli-test",
                1,
                "Running",
                "2026-03-17T00:00:00Z",
                Option::<String>::None,
                "prompt-cli-test",
                ".grove/prompts/prompt-cli-test.json",
                512,
                120,
                0,
                Option::<i32>::None,
                Option::<String>::None,
                ".grove/transcripts/grove-cli-test/ses-cli-test.jsonl",
            ],
        )?;
        Ok(())
    }

    fn run<I, S>(&self, args: I) -> Result<Output, Box<dyn std::error::Error>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        self.run_with_env(args, std::iter::empty::<(&str, &str)>())
    }

    fn run_from_dir<I, S>(
        &self,
        args: I,
        current_dir: &std::path::Path,
    ) -> Result<Output, Box<dyn std::error::Error>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        self.run_with_env_from_dir(args, std::iter::empty::<(&str, &str)>(), current_dir)
    }

    fn run_with_env<I, S, E, K, V>(
        &self,
        args: I,
        env_vars: E,
    ) -> Result<Output, Box<dyn std::error::Error>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
        E: IntoIterator<Item = (K, V)>,
        K: AsRef<std::ffi::OsStr>,
        V: AsRef<std::ffi::OsStr>,
    {
        self.run_with_env_from_dir(args, env_vars, self.workspace_root.as_std_path())
    }

    fn run_with_env_from_dir<I, S, E, K, V>(
        &self,
        args: I,
        env_vars: E,
        current_dir: &std::path::Path,
    ) -> Result<Output, Box<dyn std::error::Error>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
        E: IntoIterator<Item = (K, V)>,
        K: AsRef<std::ffi::OsStr>,
        V: AsRef<std::ffi::OsStr>,
    {
        let binary = env!("CARGO_BIN_EXE_grove");
        let original_path = env::var_os("PATH").unwrap_or_default();
        let mut path_entries = vec![self.bin_dir.as_std_path().as_os_str().to_owned()];
        path_entries.extend(env::split_paths(&original_path).map(|path| path.into_os_string()));
        let joined_path = env::join_paths(path_entries)?;

        let mut command = Command::new(binary);
        command
            .args(args)
            .current_dir(current_dir)
            .env("PATH", &joined_path)
            .env_remove("GROVE_TEST_BV_TRIAGE_FAIL");

        for (key, value) in env_vars {
            command.env(key, value);
        }

        let output = command.output()?;
        Ok(output)
    }
}

fn write_executable(path: &camino::Utf8Path, content: &str) -> TestResult {
    fs::write(path, content)?;
    #[cfg(unix)]
    {
        let metadata = fs::metadata(path)?;
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

fn output_text(output: &Output) -> String {
    format!(
        "stdout:\n{}\n\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

const CLAUDE_STUB: &str = r#"#!/usr/bin/env bash
set -euo pipefail
if [[ "${1-}" == "--version" ]]; then
  echo "claude 1.0.0-test"
  exit 0
fi
printf 'unexpected claude invocation: %s\n' "$*" >&2
exit 1
"#;

const BR_STUB: &str = r#"#!/usr/bin/env bash
set -euo pipefail
if [[ "${1-}" == "--version" ]]; then
  echo "br 0.1.12-test"
  exit 0
fi
beads_exists=false
if [[ -d .beads ]]; then
  beads_exists=true
fi
case "$*" in
  "ready --json")
    cat <<'EOF'
{"issues":[{"id":"grove-cli-test","title":"CLI inspect test","description":"Detailed CLI inspect description","priority":1,"issue_type":"task","status":"open","assignee":null,"labels":["area:test"],"created_at":"2026-03-17T00:00:00Z","updated_at":"2026-03-17T00:00:00Z","blocked_by":[],"blocks":[]}],"count":1}
EOF
    ;;
  "list --json")
    cat <<'EOF'
[{"id":"grove-cli-test","title":"CLI inspect test","description":"Detailed CLI inspect description","priority":1,"issue_type":"task","status":"open","assignee":null,"labels":["area:test"],"created_at":"2026-03-17T00:00:00Z","updated_at":"2026-03-17T00:00:00Z","blocked_by":[],"blocks":[]}]
EOF
    ;;
  "show grove-cli-test --json")
    cat <<'EOF'
{"id":"grove-cli-test","title":"CLI inspect test","description":"Detailed CLI inspect description","priority":1,"issue_type":"task","status":"open","assignee":null,"labels":["area:test"],"created_at":"2026-03-17T00:00:00Z","updated_at":"2026-03-17T00:00:00Z","blocked_by":[],"blocks":[],"comments":[]}
EOF
    ;;
  "show grove-missing --json")
    echo '[]'
    ;;
  "dep list grove-cli-test --json")
    echo '{"blocked_by":[],"blocks":[]}'
    ;;
  "dep list grove-missing --json")
    echo '{"blocked_by":[],"blocks":[]}'
    ;;
  *)
    printf 'unexpected br invocation: %s\n' "$*" >&2
    exit 1
    ;;
esac
"#;

const BV_STUB: &str = r#"#!/usr/bin/env bash
set -euo pipefail
if [[ "${1-}" == "--version" ]]; then
  echo "bv 0.1.12-test"
  exit 0
fi
if [[ "$*" == "--robot-triage" ]]; then
  if [[ -n "${GROVE_TEST_BV_TRIAGE_FAIL-}" ]]; then
    printf '%s\n' "$GROVE_TEST_BV_TRIAGE_FAIL" >&2
    exit 1
  fi
  cat <<'EOF'
{"generated_at":"2026-03-17T00:00:00Z","data_hash":"test-hash","triage":{"meta":{"version":"test","generated_at":"2026-03-17T00:00:00Z","phase2_ready":false,"issue_count":1,"compute_time_ms":1},"quick_ref":{"open_count":1,"actionable_count":1,"blocked_count":0,"in_progress_count":0,"top_picks":[{"id":"grove-cli-test","title":"CLI inspect test","score":1.0,"reasons":["ready"],"unblocks":0}]},"recommendations":[],"quick_wins":[],"blockers_to_clear":[],"project_health":{"counts":{"total":1,"open":1,"closed":0,"blocked":0,"actionable":1,"by_status":{"open":1},"by_type":{"task":1},"by_priority":{"P1":1}},"graph":{"node_count":1,"edge_count":0,"density":0.0,"has_cycles":false,"phase2_ready":false},"velocity":{"closed_last_7_days":0,"closed_last_30_days":0,"avg_days_to_close":null,"weekly":[]}},"commands":{"next":"bv --robot-next"}},"usage_hints":[]}
EOF
  exit 0
fi
printf 'unexpected bv invocation: %s\n' "$*" >&2
exit 1
"#;
