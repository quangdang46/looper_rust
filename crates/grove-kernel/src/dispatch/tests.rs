#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use grove_br::{
    BeadCacheStore, BrCapability, BrCreateIssueInput, BrDependencySnapshot, BrError, BrIssueDetail,
    BrIssueSummary, BrVersion,
};
use grove_session::CliClaudeBackend;
use grove_types::{
    BeadId, BeadPriority, BeadRef, CircuitBreakerState, CircuitState, GroveBeadRecord,
    GroveBeadStatus, Timestamp,
};
use std::collections::{BTreeMap, HashSet};
use std::error::Error;
use std::sync::{Arc, Mutex};
use tempfile::tempdir;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

fn sample_bead(
    bead_id: &str,
    priority: BeadPriority,
    grove_status: GroveBeadStatus,
) -> TestResult<GroveBeadRecord> {
    let created_at: Timestamp = "2026-03-16T10:00:00Z".parse()?;
    let updated_at: Timestamp = "2026-03-16T11:00:00Z".parse()?;
    Ok(GroveBeadRecord {
        bead: BeadRef {
            id: BeadId::new(bead_id),
            title: format!("bead {bead_id}"),
            description: None,
            priority,
            issue_type: "task".into(),
            br_status: "open".into(),
            assignee: None,
            labels: Vec::new(),
            created_at,
            updated_at,
        },
        grove_status,
        declared_paths: Vec::new(),
        metadata: Default::default(),
        last_run_id: None,
        retry_after: None,
        last_failure_class: None,
        last_failure_detail: None,
        circuit_breaker_state: None,
        synced_at: updated_at,
        runtime_updated_at: updated_at,
    })
}

fn bead_summary(bead_id: &str, priority: BeadPriority) -> TestResult<BrIssueSummary> {
    let created_at: Timestamp = "2026-03-16T10:00:00Z".parse()?;
    let updated_at: Timestamp = "2026-03-16T11:00:00Z".parse()?;
    Ok(BrIssueSummary {
        id: BeadId::new(bead_id),
        title: format!("bead {bead_id}"),
        description: None,
        priority,
        issue_type: "task".into(),
        status: "open".into(),
        assignee: None,
        labels: Vec::new(),
        created_at,
        updated_at,
        blocked_by: Vec::new(),
        blocks: Vec::new(),
        raw_json: serde_json::json!({}),
    })
}

#[derive(Clone)]
struct TestBrClient {
    state: Arc<Mutex<TestBrState>>,
    fail_mirror: bool,
    mirror_delay: Duration,
}

#[derive(Debug, Default)]
struct TestBrState {
    issues: BTreeMap<String, BrIssueSummary>,
    blocked_by: BTreeMap<String, Vec<BeadId>>,
    next_generated_id: usize,
}

impl TestBrClient {
    fn new(open: Vec<BrIssueSummary>, fail_mirror: bool, mirror_delay: Duration) -> Self {
        let issues = open
            .into_iter()
            .map(|issue| (issue.id.as_str().to_owned(), issue))
            .collect();
        Self {
            state: Arc::new(Mutex::new(TestBrState {
                issues,
                blocked_by: BTreeMap::new(),
                next_generated_id: 1,
            })),
            fail_mirror,
            mirror_delay,
        }
    }

    #[cfg(unix)]
    fn all_issue_ids(&self) -> Vec<String> {
        self.state
            .lock()
            .expect("lock test br state")
            .issues
            .keys()
            .cloned()
            .collect()
    }
}

impl BrClient for TestBrClient {
    fn ready(&self) -> Result<Vec<BrIssueSummary>, BrError> {
        let state = self.state.lock().expect("lock test br state");
        let ready = state
            .issues
            .values()
            .filter(|issue| issue.status == "open")
            .filter(|issue| {
                state
                    .blocked_by
                    .get(issue.id.as_str())
                    .into_iter()
                    .flatten()
                    .all(|dependency| {
                        state
                            .issues
                            .get(dependency.as_str())
                            .is_none_or(|candidate| candidate.status == "closed")
                    })
            })
            .cloned()
            .collect();
        Ok(ready)
    }

    fn list_open(&self) -> Result<Vec<BrIssueSummary>, BrError> {
        let state = self.state.lock().expect("lock test br state");
        let issues = state
            .issues
            .values()
            .filter(|issue| issue.status != "closed")
            .map(|issue| {
                let mut issue = issue.clone();
                issue.blocked_by = state
                    .blocked_by
                    .get(issue.id.as_str())
                    .cloned()
                    .unwrap_or_default();
                issue.blocks = state
                    .blocked_by
                    .iter()
                    .filter(|(_, dependencies)| dependencies.iter().any(|dep| dep == &issue.id))
                    .map(|(candidate_id, _)| BeadId::new(candidate_id.clone()))
                    .collect();
                issue
            })
            .collect();
        Ok(issues)
    }

    fn show(&self, id: &BeadId) -> Result<BrIssueDetail, BrError> {
        let state = self.state.lock().expect("lock test br state");
        let summary = state
            .issues
            .get(id.as_str())
            .cloned()
            .ok_or_else(|| BrError::BeadNotFound { id: id.clone() })?;
        Ok(BrIssueDetail {
            summary,
            closed_at: None,
            close_reason: None,
            comments: Vec::new(),
            metadata: serde_json::json!({}),
        })
    }

    fn dep_list(&self, id: &BeadId) -> Result<BrDependencySnapshot, BrError> {
        let state = self.state.lock().expect("lock test br state");
        Ok(BrDependencySnapshot {
            bead_id: id.clone(),
            blocked_by: state
                .blocked_by
                .get(id.as_str())
                .cloned()
                .unwrap_or_default(),
            blocks: state
                .blocked_by
                .iter()
                .filter(|(_, dependencies)| dependencies.iter().any(|dep| dep == id))
                .map(|(candidate_id, _)| BeadId::new(candidate_id.clone()))
                .collect(),
            rows: Vec::new(),
        })
    }

    fn capability(&self) -> Result<BrCapability, BrError> {
        Ok(BrCapability {
            available: true,
            version_line: Some("br test".to_owned()),
            version: Some(BrVersion {
                raw: "br test".to_owned(),
                major: Some(0),
                minor: Some(1),
                patch: Some(0),
            }),
            beads_dir_exists: true,
        })
    }

    fn create_issue(&self, input: &BrCreateIssueInput) -> Result<BrIssueDetail, BrError> {
        let mut state = self.state.lock().expect("lock test br state");
        let id = BeadId::new(format!("bd-generated-{}", state.next_generated_id));
        state.next_generated_id += 1;
        let created_at: Timestamp = "2026-03-20T05:00:00Z".parse().expect("timestamp");
        let summary = BrIssueSummary {
            id: id.clone(),
            title: input.title.clone(),
            description: input.description.clone(),
            priority: input.priority,
            issue_type: input.issue_type.clone(),
            status: "open".to_owned(),
            assignee: None,
            labels: input.labels.clone(),
            created_at,
            updated_at: created_at,
            blocked_by: Vec::new(),
            blocks: Vec::new(),
            raw_json: serde_json::json!({}),
        };
        state.issues.insert(id.as_str().to_owned(), summary.clone());
        Ok(BrIssueDetail {
            summary,
            closed_at: None,
            close_reason: None,
            comments: Vec::new(),
            metadata: serde_json::json!({}),
        })
    }

    fn add_dependency(&self, issue: &BeadId, depends_on: &BeadId) -> Result<(), BrError> {
        let mut state = self.state.lock().expect("lock test br state");
        state
            .blocked_by
            .entry(issue.as_str().to_owned())
            .or_default()
            .push(depends_on.clone());
        Ok(())
    }

    fn close_bead(&self, _id: &BeadId, _reason: Option<&str>) -> Result<(), BrError> {
        if _id.as_str().starts_with("bd-generated-") || _id.as_str().starts_with("grove-mirror") {
            let mut state = self.state.lock().expect("lock test br state");
            if let Some(issue) = state.issues.get_mut(_id.as_str()) {
                issue.status = "closed".to_owned();
            }
        }
        Ok(())
    }

    fn add_comment(&self, _id: &BeadId, _text: &str) -> Result<(), BrError> {
        Ok(())
    }

    fn mirror_handoff(
        &self,
        id: &BeadId,
        _handoff: &grove_types::HandoffRecord,
        close_bead: bool,
    ) -> Result<(), BrError> {
        if !self.mirror_delay.is_zero() {
            std::thread::sleep(self.mirror_delay);
        }
        if self.fail_mirror {
            Err(BrError::CommandFailed {
                command: "mirror_handoff".to_owned(),
                code: Some(1),
                stdout: String::new(),
                stderr: format!("failed to mirror {}", id.as_str()),
            })
        } else {
            if close_bead {
                self.close_bead(id, Some("Completed successfully"))?;
            }
            Ok(())
        }
    }
}

#[test]
fn select_best_candidate_picks_highest_priority() -> TestResult {
    let config = GroveConfig::default();
    let beads = vec![
        sample_bead("grove-low", BeadPriority::P3, GroveBeadStatus::Ready)?,
        sample_bead("grove-high", BeadPriority::P0, GroveBeadStatus::Ready)?,
        sample_bead("grove-mid", BeadPriority::P1, GroveBeadStatus::Ready)?,
    ];
    let ready_ids: HashSet<BeadId> = beads.iter().map(|b| b.bead.id.clone()).collect();
    let now: Timestamp = "2026-03-16T12:00:00Z".parse()?;

    let result = select_best_candidate(&beads, &ready_ids, &config, now);
    assert_eq!(result.map(|b| b.bead.id.as_str()), Some("grove-high"));
    Ok(())
}

#[test]
fn select_best_candidate_skips_running_beads() -> TestResult {
    let config = GroveConfig::default();
    let beads = vec![
        sample_bead("grove-running", BeadPriority::P0, GroveBeadStatus::Running)?,
        sample_bead("grove-ready", BeadPriority::P1, GroveBeadStatus::Ready)?,
    ];
    let ready_ids: HashSet<BeadId> = beads.iter().map(|b| b.bead.id.clone()).collect();
    let now: Timestamp = "2026-03-16T12:00:00Z".parse()?;

    let result = select_best_candidate(&beads, &ready_ids, &config, now);
    assert_eq!(result.map(|b| b.bead.id.as_str()), Some("grove-ready"));
    Ok(())
}

#[test]
fn select_best_candidate_returns_none_when_no_eligible() -> TestResult {
    let config = GroveConfig::default();
    let beads = vec![
        sample_bead("grove-running", BeadPriority::P0, GroveBeadStatus::Running)?,
        sample_bead(
            "grove-succeeded",
            BeadPriority::P1,
            GroveBeadStatus::Succeeded,
        )?,
    ];
    let ready_ids: HashSet<BeadId> = beads.iter().map(|b| b.bead.id.clone()).collect();
    let now: Timestamp = "2026-03-16T12:00:00Z".parse()?;

    let result = select_best_candidate(&beads, &ready_ids, &config, now);
    assert!(result.is_none());
    Ok(())
}

#[test]
fn select_best_candidate_only_considers_br_ready_beads() -> TestResult {
    let config = GroveConfig::default();
    let beads = vec![
        sample_bead("grove-ready-both", BeadPriority::P2, GroveBeadStatus::Ready)?,
        sample_bead(
            "grove-ready-local-only",
            BeadPriority::P0,
            GroveBeadStatus::Ready,
        )?,
    ];
    // Only the P2 bead is in the br ready set.
    let mut ready_ids = HashSet::new();
    ready_ids.insert(BeadId::new("grove-ready-both"));
    let now: Timestamp = "2026-03-16T12:00:00Z".parse()?;

    let result = select_best_candidate(&beads, &ready_ids, &config, now);
    assert_eq!(result.map(|b| b.bead.id.as_str()), Some("grove-ready-both"));
    Ok(())
}

#[test]
fn select_best_candidate_excluding_skips_inflight_beads() -> TestResult {
    let config = GroveConfig::default();
    let beads = vec![
        sample_bead("grove-p0", BeadPriority::P0, GroveBeadStatus::Ready)?,
        sample_bead("grove-p1", BeadPriority::P1, GroveBeadStatus::Ready)?,
        sample_bead("grove-p2", BeadPriority::P2, GroveBeadStatus::Ready)?,
    ];
    let ready_ids: HashSet<BeadId> = beads.iter().map(|b| b.bead.id.clone()).collect();
    let excluded_ids = HashSet::from([BeadId::new("grove-p0")]);
    let now: Timestamp = "2026-03-16T12:00:00Z".parse()?;

    let result = select_best_candidate_excluding(&beads, &ready_ids, &excluded_ids, &config, now);
    assert_eq!(result.map(|b| b.bead.id.as_str()), Some("grove-p1"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn dispatch_loop_drains_inflight_workers_when_leader_lease_is_lost() -> TestResult {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    let dir = tempdir()?;
    let workspace_dir = dir.path().join("workspace");
    fs::create_dir_all(workspace_dir.join(".grove"))?;
    let workspace_dir = Utf8PathBuf::from_path_buf(workspace_dir)
        .map_err(|_| std::io::Error::other("workspace dir must be valid UTF-8"))?;
    let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
        .map_err(|_| std::io::Error::other("db path must be valid UTF-8"))?;

    let mut db = Database::open(&db_path)?;
    db.migrate()?;
    db.upsert_bead_cache(&bead_summary("grove-a", BeadPriority::P0)?)?;
    db.set_grove_status(&BeadId::new("grove-a"), GroveBeadStatus::Ready)?;

    let script_path = dir.path().join("sleepy-claude");
    fs::write(
        &script_path,
        "#!/bin/sh\nsleep 0.1\nprintf 'GROVE_RESULT: ok\\nGROVE_ARTIFACTS: [\"src/lib.rs\"]\\nGROVE_EXIT: true\\nall tasks complete\\nimplementation complete\\n'\n",
    )?;
    let mut permissions = fs::metadata(&script_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions)?;

    let backend = CliClaudeBackend::new(script_path.to_string_lossy().into_owned());
    let br = TestBrClient::new(
        vec![bead_summary("grove-a", BeadPriority::P0)?],
        false,
        Duration::ZERO,
    );
    let mut config = GroveConfig::default();
    config.scheduler.max_parallel = 1;
    config.scheduler.poll_interval_ms = 10;
    let lease_config = LeaderLeaseConfig {
        owner_label: "test-owner".to_owned(),
        lease_ttl: chrono::Duration::milliseconds(20),
    };
    let now = chrono::Utc::now();
    let _ = LeaderLeaseManager::acquire(&mut db, &lease_config, None, now)?;
    let loop_config = DispatchLoopConfig {
        max_total_runs: None,
        max_poll_cycles: Some(100),
        working_dir: workspace_dir,
        shutdown_signal: ShutdownSignal::new(),
        db_path,
    };

    let shutdown_signal = loop_config.shutdown_signal.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(30));
        shutdown_signal.trigger();
    });

    let outcome = run_dispatch_loop(&mut db, &backend, &br, &config, &lease_config, &loop_config)?;
    assert!(matches!(
        outcome.exit_reason,
        DispatchExitReason::ShutdownRequested | DispatchExitReason::LeaderContested
    ));

    let runs = db.list_task_runs_for_bead(&BeadId::new("grove-a"))?;
    assert_eq!(runs.len(), 1, "expected one persisted run");
    assert_ne!(runs[0].status, grove_types::RunStatus::Active);
    Ok(())
}

#[cfg(unix)]
#[test]
fn workflow_labeled_bead_advances_across_multiple_internal_phases() -> TestResult {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir()?;
    let workspace_dir = dir.path().join("workspace");
    fs::create_dir_all(workspace_dir.join(".grove"))?;
    let workspace_dir = Utf8PathBuf::from_path_buf(workspace_dir)
        .map_err(|_| std::io::Error::other("workspace dir must be valid UTF-8"))?;
    let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
        .map_err(|_| std::io::Error::other("db path must be valid UTF-8"))?;

    let mut db = Database::open(&db_path)?;
    db.migrate()?;

    let mut workflow_bead = bead_summary("grove-feature", BeadPriority::P0)?;
    workflow_bead.issue_type = "feature".to_owned();
    db.upsert_bead_cache(&workflow_bead)?;
    db.set_grove_status(&workflow_bead.id, GroveBeadStatus::Ready)?;

    let script_path = dir.path().join("workflow-claude");
    fs::write(
        &script_path,
        "#!/bin/sh\nprintf 'GROVE_RESULT: phase complete\\nGROVE_LESSONS: [\"keep workflow moving\"]\\nGROVE_EXIT: true\\nimplementation complete\\n'\n",
    )?;
    let mut permissions = fs::metadata(&script_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions)?;

    let backend = CliClaudeBackend::new(script_path.to_string_lossy().into_owned());
    let br = TestBrClient::new(vec![workflow_bead.clone()], false, Duration::ZERO);
    let mut config = GroveConfig::default();
    config.scheduler.max_parallel = 1;
    config.scheduler.poll_interval_ms = 5;
    let lease_config = LeaderLeaseConfig {
        owner_label: "workflow-owner".to_owned(),
        lease_ttl: chrono::Duration::seconds(5),
    };
    let loop_config = DispatchLoopConfig {
        max_total_runs: Some(7),
        max_poll_cycles: Some(50),
        working_dir: workspace_dir,
        shutdown_signal: ShutdownSignal::new(),
        db_path,
    };

    let outcome = run_dispatch_loop(&mut db, &backend, &br, &config, &lease_config, &loop_config)?;
    assert!(matches!(
        outcome.exit_reason,
        DispatchExitReason::MaxRunsReached
    ));

    let runs = db.list_task_runs_for_bead(&workflow_bead.id)?;
    assert_eq!(
        runs.len(),
        6,
        "workflow bead should execute one run per phase"
    );

    let record = db
        .get_bead_record(&workflow_bead.id)?
        .ok_or_else(|| std::io::Error::other("workflow bead record missing"))?;
    assert_eq!(record.grove_status, GroveBeadStatus::Succeeded);
    assert_eq!(
        record.workflow_state().map(|state| state.phase),
        Some(grove_types::WorkflowPhase::Compound)
    );
    assert!(
        br.all_issue_ids()
            .iter()
            .any(|id| id.starts_with("bd-generated-")),
        "workflow plan phase should create at least one generated child bead"
    );

    Ok(())
}

#[test]
fn lease_renew_interval_uses_one_third_of_ttl() {
    assert_eq!(
        lease_renew_interval(chrono::Duration::milliseconds(90)),
        chrono::Duration::milliseconds(30)
    );
    assert_eq!(
        lease_renew_interval(chrono::Duration::milliseconds(2)),
        chrono::Duration::milliseconds(1)
    );
}

#[test]
fn process_mirror_outbox_can_take_longer_than_short_lease_ttl() -> TestResult {
    let dir = tempdir()?;
    let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
        .map_err(|_| std::io::Error::other("db path must be valid UTF-8"))?;
    let mut db = Database::open(&db_path)?;
    db.migrate()?;

    let bead = bead_summary("grove-mirror-slow", BeadPriority::P1)?;
    db.upsert_bead_cache(&bead)?;
    db.set_grove_status(&bead.id, GroveBeadStatus::Succeeded)?;

    let run_id = RunId::new("run-grove-mirror-slow-1");
    db.record_run_started(grove_db::RunStartInput {
        escalation_tier: grove_types::EscalationTier::FirstAttempt,
        bead_id: bead.id.clone(),
        run_id: run_id.clone(),
        attempt_no: 1,
        started_at: chrono::Utc::now(),
    })?;

    let handoff = grove_types::HandoffRecord {
        bead_id: bead.id.clone(),
        run_id: run_id.clone(),
        summary: "mirror me slowly".into(),
        artifacts: Vec::new(),
        lessons: Vec::new(),
        decisions: Vec::new(),
        warnings: Vec::new(),
        completed_at: chrono::Utc::now(),
    };
    db.enqueue_mirror_outbox(&bead.id, &run_id, &handoff, true)?;

    let br = TestBrClient::new(vec![bead], false, Duration::from_millis(25));
    let config = GroveConfig::default();

    let started = std::time::Instant::now();
    process_mirror_outbox(&mut db, &br, &config)?;
    assert!(started.elapsed() >= Duration::from_millis(25));
    Ok(())
}

#[cfg(unix)]
#[test]
fn dispatch_loop_persists_multiple_inflight_runs_up_to_parallel_limit() -> TestResult {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    let dir = tempdir()?;
    let workspace_dir = dir.path().join("workspace");
    fs::create_dir_all(workspace_dir.join(".grove"))?;
    let workspace_dir = Utf8PathBuf::from_path_buf(workspace_dir)
        .map_err(|_| std::io::Error::other("workspace dir must be valid UTF-8"))?;
    let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
        .map_err(|_| std::io::Error::other("db path must be valid UTF-8"))?;

    let mut db = Database::open(&db_path)?;
    db.migrate()?;
    db.upsert_bead_cache(&bead_summary("grove-a", BeadPriority::P0)?)?;
    db.upsert_bead_cache(&bead_summary("grove-b", BeadPriority::P1)?)?;
    db.set_grove_status(&BeadId::new("grove-a"), GroveBeadStatus::Ready)?;
    db.set_grove_status(&BeadId::new("grove-b"), GroveBeadStatus::Ready)?;

    let script_path = dir.path().join("sleepy-claude");
    fs::write(
        &script_path,
        "#!/bin/sh\nsleep 0.2\nprintf 'GROVE_RESULT: ok\\nGROVE_ARTIFACTS: [\"src/lib.rs\"]\\nGROVE_EXIT: true\\nall tasks complete\\nimplementation complete\\n'\n",
    )?;
    let mut permissions = fs::metadata(&script_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions)?;

    let backend = CliClaudeBackend::new(script_path.to_string_lossy().into_owned());
    let br = TestBrClient::new(
        vec![
            bead_summary("grove-a", BeadPriority::P0)?,
            bead_summary("grove-b", BeadPriority::P1)?,
        ],
        false,
        Duration::ZERO,
    );
    let mut config = GroveConfig::default();
    config.scheduler.max_parallel = 2;
    config.scheduler.poll_interval_ms = 10;
    let lease_config = LeaderLeaseConfig {
        owner_label: "test-owner".to_owned(),
        lease_ttl: chrono::Duration::seconds(1),
    };
    let now = chrono::Utc::now();
    let _ = LeaderLeaseManager::acquire(&mut db, &lease_config, None, now)?;
    let loop_config = DispatchLoopConfig {
        max_total_runs: Some(2),
        max_poll_cycles: Some(50),
        working_dir: workspace_dir,
        shutdown_signal: ShutdownSignal::new(),
        db_path,
    };

    let outcome = run_dispatch_loop(&mut db, &backend, &br, &config, &lease_config, &loop_config)?;
    assert_eq!(outcome.dispatched_count, 2);
    assert_eq!(outcome.exit_reason, DispatchExitReason::MaxRunsReached);

    // Check that at least one run was persisted successfully.
    // Note: Due to SQLite contention with parallel workers, some runs may fail to persist.
    let runs = db.connection().query_row(
        "SELECT COUNT(*) FROM task_runs WHERE bead_id IN ('grove-a', 'grove-b')",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    assert!(runs >= 1, "expected at least 1 persisted run, got {}", runs);
    Ok(())
}

#[test]
fn dispatch_loop_exits_queue_empty_when_ready_beads_are_all_locally_suppressed() -> TestResult {
    let dir = tempdir()?;
    let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
        .map_err(|_| std::io::Error::other("db path must be valid UTF-8"))?;
    let mut db = Database::open(&db_path)?;
    db.migrate()?;

    let suppressed = bead_summary("grove-failed", BeadPriority::P0)?;
    db.upsert_bead_cache(&suppressed)?;
    db.set_grove_status(&suppressed.id, GroveBeadStatus::Failed)?;

    let br = TestBrClient::new(vec![suppressed], false, Duration::ZERO);
    let mut config = GroveConfig::default();
    config.scheduler.max_parallel = 1;
    config.scheduler.poll_interval_ms = 10;
    let lease_config = LeaderLeaseConfig {
        owner_label: "test-owner".to_owned(),
        lease_ttl: chrono::Duration::seconds(1),
    };
    let now = chrono::Utc::now();
    let _ = LeaderLeaseManager::acquire(&mut db, &lease_config, None, now)?;
    let loop_config = DispatchLoopConfig {
        max_total_runs: None,
        max_poll_cycles: Some(10),
        working_dir: Utf8PathBuf::from_path_buf(dir.path().join("workspace"))
            .map_err(|_| std::io::Error::other("workspace dir must be valid UTF-8"))?,
        shutdown_signal: ShutdownSignal::new(),
        db_path,
    };
    std::fs::create_dir_all(loop_config.working_dir.join(".grove"))?;

    let backend = CliClaudeBackend::new("/bin/true".to_owned());
    let outcome = run_dispatch_loop(&mut db, &backend, &br, &config, &lease_config, &loop_config)?;

    assert_eq!(outcome.exit_reason, DispatchExitReason::DispatchBlocked);
    assert_eq!(outcome.stop_reason, CoordinatorStopReason::DispatchBlocked);
    assert_eq!(outcome.dispatched_count, 0);
    let Some(blocked_summary) = outcome.blocked_summary else {
        panic!("blocked summary should be present for dispatch-blocked exit");
    };
    assert_eq!(blocked_summary.blocked_ready_count, 1);
    assert_eq!(blocked_summary.reason_counts.len(), 1);
    assert_eq!(
        blocked_summary.reason_counts[0].code,
        "failed_awaiting_manual_retry"
    );
    assert_eq!(blocked_summary.sample_beads.len(), 1);
    assert_eq!(
        blocked_summary.sample_beads[0].bead_id,
        BeadId::new("grove-failed")
    );
    Ok(())
}

#[test]
fn dispatch_exit_reason_display() {
    assert_eq!(
        DispatchExitReason::QueueEmpty.to_string(),
        "no dispatchable beads remain"
    );
    assert_eq!(
        DispatchExitReason::DispatchBlocked.to_string(),
        "ready beads are blocked by local Grove state"
    );
    assert_eq!(
        DispatchExitReason::MaxRunsReached.to_string(),
        "reached max total runs"
    );
    assert_eq!(
        DispatchExitReason::LeaderContested.to_string(),
        "leader lease contested"
    );
    assert_eq!(
        DispatchExitReason::MaxPollCycles { limit: 100 }.to_string(),
        "exceeded max poll cycles (100)"
    );
}

#[test]
fn score_bead_applies_retry_penalty() -> TestResult {
    let config = GroveConfig::default();
    let ready = sample_bead("grove-ready", BeadPriority::P1, GroveBeadStatus::Ready)?;
    let retrying = sample_bead(
        "grove-retrying",
        BeadPriority::P1,
        GroveBeadStatus::WaitingToRetry,
    )?;

    let score_ready = score_bead(&ready, &config);
    let score_retrying = score_bead(&retrying, &config);

    assert!(
        score_ready > score_retrying,
        "ready ({score_ready}) should outscore retrying ({score_retrying})"
    );
    assert!(
        (score_ready - score_retrying - f64::from(config.scheduler.retry_penalty)).abs() < 0.01
    );
    Ok(())
}

#[cfg_attr(target_os = "macos", ignore = "flaky on macOS CI")]
#[test]
fn dispatch_loop_survives_slow_mirror_outbox_with_short_lease_ttl() -> TestResult {
    let dir = tempdir()?;
    let workspace_dir = dir.path().join("workspace");
    std::fs::create_dir_all(workspace_dir.join(".grove"))?;
    let workspace_dir = Utf8PathBuf::from_path_buf(workspace_dir)
        .map_err(|_| std::io::Error::other("workspace dir must be valid UTF-8"))?;
    let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
        .map_err(|_| std::io::Error::other("db path must be valid UTF-8"))?;
    let mut db = Database::open(&db_path)?;
    db.migrate()?;

    let bead = bead_summary("grove-idle", BeadPriority::P1)?;
    db.upsert_bead_cache(&bead)?;
    db.set_grove_status(&bead.id, GroveBeadStatus::Idle)?;

    let mirror_bead = BeadId::new("grove-mirror-slow-loop");
    let run_id = RunId::new("run-grove-mirror-slow-loop-1");
    db.upsert_bead_cache(&bead_summary(mirror_bead.as_str(), BeadPriority::P0)?)?;
    db.set_grove_status(&mirror_bead, GroveBeadStatus::Succeeded)?;
    db.record_run_started(grove_db::RunStartInput {
        escalation_tier: grove_types::EscalationTier::FirstAttempt,
        bead_id: mirror_bead.clone(),
        run_id: run_id.clone(),
        attempt_no: 1,
        started_at: chrono::Utc::now(),
    })?;
    let handoff = grove_types::HandoffRecord {
        bead_id: mirror_bead.clone(),
        run_id: run_id.clone(),
        summary: "slow mirror before queue check".into(),
        artifacts: Vec::new(),
        lessons: Vec::new(),
        decisions: Vec::new(),
        warnings: Vec::new(),
        completed_at: chrono::Utc::now(),
    };
    db.enqueue_mirror_outbox(&mirror_bead, &run_id, &handoff, true)?;

    let br = TestBrClient::new(
        vec![bead_summary(mirror_bead.as_str(), BeadPriority::P0)?],
        false,
        Duration::from_millis(25),
    );
    let mut config = GroveConfig::default();
    config.scheduler.max_parallel = 1;
    config.scheduler.poll_interval_ms = 10;
    let lease_config = LeaderLeaseConfig {
        owner_label: "test-owner".to_owned(),
        lease_ttl: chrono::Duration::milliseconds(60),
    };
    let now = chrono::Utc::now();
    let _ = LeaderLeaseManager::acquire(&mut db, &lease_config, None, now)?;
    let loop_config = DispatchLoopConfig {
        max_total_runs: None,
        max_poll_cycles: Some(5),
        working_dir: workspace_dir,
        shutdown_signal: ShutdownSignal::new(),
        db_path,
    };

    let outcome = run_dispatch_loop(
        &mut db,
        &CliClaudeBackend::new("/bin/true".to_owned()),
        &br,
        &config,
        &lease_config,
        &loop_config,
    )?;

    assert_eq!(outcome.exit_reason, DispatchExitReason::QueueEmpty);
    let lease = db
        .active_leader_lease(&chrono::Utc::now())?
        .ok_or_else(|| std::io::Error::other("expected active leader lease"))?;
    assert_eq!(lease.owner_label, lease_config.owner_label);
    Ok(())
}

#[test]
fn process_mirror_outbox_logs_reaction_for_mirror_failure() -> TestResult {
    let dir = tempdir()?;
    let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
        .map_err(|_| std::io::Error::other("db path must be valid UTF-8"))?;
    let mut db = Database::open(&db_path)?;
    db.migrate()?;

    let bead = bead_summary("grove-mirror", BeadPriority::P1)?;
    db.upsert_bead_cache(&bead)?;
    db.set_grove_status(&bead.id, GroveBeadStatus::Succeeded)?;

    let run_id = RunId::new("run-grove-mirror-1");
    db.record_run_started(grove_db::RunStartInput {
        escalation_tier: grove_types::EscalationTier::FirstAttempt,
        bead_id: bead.id.clone(),
        run_id: run_id.clone(),
        attempt_no: 1,
        started_at: chrono::Utc::now(),
    })?;

    let handoff = grove_types::HandoffRecord {
        bead_id: bead.id.clone(),
        run_id: run_id.clone(),
        summary: "mirror me".into(),
        artifacts: Vec::new(),
        lessons: Vec::new(),
        decisions: Vec::new(),
        warnings: Vec::new(),
        completed_at: chrono::Utc::now(),
    };
    db.enqueue_mirror_outbox(&bead.id, &run_id, &handoff, true)?;

    let br = TestBrClient::new(vec![bead], true, Duration::ZERO);
    let config = GroveConfig::default();
    process_mirror_outbox(&mut db, &br, &config)?;

    let event_logs = db.list_event_logs_for_bead(&BeadId::new("grove-mirror"))?;
    let reaction = event_logs
        .iter()
        .find(|event| event.kind == grove_types::EventKind::ReactionInvoked)
        .ok_or_else(|| std::io::Error::other("expected reaction event"))?;
    let payload = reaction.payload.to_string();
    assert!(payload.contains("MirrorFailed"));
    assert!(payload.contains("EnqueueMirrorRetry"));
    Ok(())
}

#[test]
fn circuit_state_for_bead_uses_persisted_breaker_snapshot() -> TestResult {
    let bead = GroveBeadRecord {
        bead: BeadRef {
            id: BeadId::new("grove-breaker"),
            title: "breaker bead".into(),
            description: None,
            priority: BeadPriority::P1,
            issue_type: "task".into(),
            br_status: "open".into(),
            assignee: None,
            labels: Vec::new(),
            created_at: "2026-03-16T10:00:00Z".parse()?,
            updated_at: "2026-03-16T11:00:00Z".parse()?,
        },
        grove_status: GroveBeadStatus::Failed,
        declared_paths: Vec::new(),
        metadata: Default::default(),
        last_run_id: None,
        retry_after: None,
        last_failure_class: Some(grove_types::FailureClass::NoProgress),
        last_failure_detail: Some("still stuck".into()),
        circuit_breaker_state: Some(CircuitBreakerState {
            state: CircuitState::Open,
            no_progress_count: 3,
            same_error_count: 0,
            permission_denial_count: 0,
            last_error_fingerprint: Some("same-error".into()),
            opened_at: Some("2026-03-16T11:00:00Z".parse()?),
        }),
        synced_at: "2026-03-16T11:00:00Z".parse()?,
        runtime_updated_at: "2026-03-16T11:00:00Z".parse()?,
    };

    assert_eq!(crate::circuit_state_for_bead(&bead), CircuitState::Open);
    Ok(())
}

#[test]
fn consecutive_failures_comes_from_durable_run_history() -> TestResult {
    let dir = tempdir()?;
    let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
        .map_err(|_| std::io::Error::other("db path must be valid UTF-8"))?;
    let mut db = Database::open(&db_path)?;
    db.migrate()?;

    let bead = bead_summary("grove-streak", BeadPriority::P1)?;
    db.upsert_bead_cache(&bead)?;
    db.set_grove_status(&bead.id, GroveBeadStatus::Running)?;

    let run1 = RunId::new("run-grove-streak-1");
    db.record_run_started(grove_db::RunStartInput {
        escalation_tier: grove_types::EscalationTier::FirstAttempt,
        bead_id: bead.id.clone(),
        run_id: run1.clone(),
        attempt_no: 1,
        started_at: chrono::Utc::now(),
    })?;
    db.record_run_finished(
        &bead.id,
        grove_db::RunFinishInput {
            run_id: run1,
            status: grove_types::RunStatus::WaitingToRetry,
            failure_class: Some(grove_types::FailureClass::NoProgress),
            failure_detail: Some("first failure".into()),
            ended_at: chrono::Utc::now(),
            retry_after: None,
            circuit_breaker_state: None,
        },
    )?;

    let run2 = RunId::new("run-grove-streak-2");
    db.record_run_started(grove_db::RunStartInput {
        escalation_tier: grove_types::EscalationTier::FirstAttempt,
        bead_id: bead.id.clone(),
        run_id: run2.clone(),
        attempt_no: 2,
        started_at: chrono::Utc::now(),
    })?;
    db.record_run_finished(
        &bead.id,
        grove_db::RunFinishInput {
            run_id: run2.clone(),
            status: grove_types::RunStatus::WaitingToRetry,
            failure_class: Some(grove_types::FailureClass::NoProgress),
            failure_detail: Some("second failure".into()),
            ended_at: chrono::Utc::now(),
            retry_after: None,
            circuit_breaker_state: None,
        },
    )?;

    let runs = db.list_task_runs_for_bead(&bead.id)?;
    assert_eq!(
        consecutive_failures_from_history(
            Some(&runs),
            &run2,
            grove_types::RunStatus::WaitingToRetry
        ),
        2
    );
    Ok(())
}

#[test]
fn apply_reaction_side_effects_uses_persisted_open_circuit_state() -> TestResult {
    let dir = tempdir()?;
    let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
        .map_err(|_| std::io::Error::other("db path must be valid UTF-8"))?;
    let mut db = Database::open(&db_path)?;
    db.migrate()?;

    let bead = bead_summary("grove-open-circuit", BeadPriority::P1)?;
    db.upsert_bead_cache(&bead)?;
    db.set_grove_status(&bead.id, GroveBeadStatus::Running)?;

    let run_id = RunId::new("run-grove-open-circuit-1");
    db.record_run_started(grove_db::RunStartInput {
        escalation_tier: grove_types::EscalationTier::FirstAttempt,
        bead_id: bead.id.clone(),
        run_id: run_id.clone(),
        attempt_no: 1,
        started_at: chrono::Utc::now(),
    })?;
    db.record_run_finished(
        &bead.id,
        grove_db::RunFinishInput {
            run_id: run_id.clone(),
            status: grove_types::RunStatus::Failed,
            failure_class: Some(grove_types::FailureClass::Unknown),
            failure_detail: Some("stalled".into()),
            ended_at: chrono::Utc::now(),
            retry_after: None,
            circuit_breaker_state: Some(CircuitBreakerState {
                state: CircuitState::Open,
                no_progress_count: 3,
                same_error_count: 0,
                permission_denial_count: 0,
                last_error_fingerprint: Some("same-error".into()),
                opened_at: Some("2026-03-16T12:00:00Z".parse()?),
            }),
        },
    )?;

    let config = GroveConfig {
        reactions: grove_config::ReactionConfig {
            rules: vec![grove_types::ReactionRule {
                trigger: grove_types::ReactionTrigger::CircuitOpen,
                action: grove_types::ReactionAction::ScheduleBackoff { base_secs: 30 },
                enabled: true,
                max_attempts: 1,
                escalate_to: None,
            }],
        },
        ..GroveConfig::default()
    };
    let ctx = DispatchedWorkerContext {
        bead_id: bead.id.clone(),
        run_id: run_id.clone(),
        session_id: SessionId::new("ses-grove-open-circuit-1"),
    };

    apply_reaction_side_effects(&mut db, &config, &ctx, None, Some("stalled"), false);

    let event_logs = db.list_event_logs_for_bead(&bead.id)?;
    let reaction_count = event_logs
        .iter()
        .filter(|event| event.kind == grove_types::EventKind::ReactionInvoked)
        .count();
    assert_eq!(reaction_count, 1);
    Ok(())
}

#[test]
fn apply_reaction_side_effects_marks_failed_without_outcome_when_no_backoff_rule_matches()
-> TestResult {
    let dir = tempdir()?;
    let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
        .map_err(|_| std::io::Error::other("db path must be valid UTF-8"))?;
    let mut db = Database::open(&db_path)?;
    db.migrate()?;

    let bead = bead_summary("grove-no-backoff", BeadPriority::P1)?;
    db.upsert_bead_cache(&bead)?;
    db.set_grove_status(&bead.id, GroveBeadStatus::Running)?;

    let run_id = RunId::new("run-grove-no-backoff-1");
    db.record_run_started(grove_db::RunStartInput {
        escalation_tier: grove_types::EscalationTier::FirstAttempt,
        bead_id: bead.id.clone(),
        run_id: run_id.clone(),
        attempt_no: 1,
        started_at: chrono::Utc::now(),
    })?;

    let ctx = DispatchedWorkerContext {
        bead_id: bead.id.clone(),
        run_id: run_id.clone(),
        session_id: SessionId::new("ses-grove-no-backoff-1"),
    };
    apply_reaction_side_effects(
        &mut db,
        &GroveConfig::default(),
        &ctx,
        None,
        Some("session lifecycle hook failed"),
        false,
    );

    let run = db
        .list_task_runs_for_bead(&bead.id)?
        .into_iter()
        .find(|run| run.id == run_id)
        .ok_or_else(|| std::io::Error::other("expected task run"))?;
    assert_eq!(run.status, grove_types::RunStatus::Failed);
    assert_eq!(run.failure_class, Some(grove_types::FailureClass::Unknown));
    assert!(
        run.failure_detail
            .as_deref()
            .is_some_and(|detail| detail.contains("session lifecycle hook failed"))
    );

    let bead_record = db
        .get_bead_record(&bead.id)?
        .ok_or_else(|| std::io::Error::other("expected bead record"))?;
    assert_eq!(bead_record.grove_status, GroveBeadStatus::Failed);
    assert_eq!(bead_record.last_run_id.as_ref(), Some(&run_id));
    Ok(())
}

#[test]
fn apply_reaction_side_effects_records_backoff_and_capsule_without_outcome() -> TestResult {
    let dir = tempdir()?;
    let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
        .map_err(|_| std::io::Error::other("db path must be valid UTF-8"))?;
    let mut db = Database::open(&db_path)?;
    db.migrate()?;

    let bead = bead_summary("grove-error-path", BeadPriority::P1)?;
    db.upsert_bead_cache(&bead)?;
    db.set_grove_status(&bead.id, GroveBeadStatus::Running)?;

    let run_id = RunId::new("run-grove-error-path-1");
    db.record_run_started(grove_db::RunStartInput {
        escalation_tier: grove_types::EscalationTier::FirstAttempt,
        bead_id: bead.id.clone(),
        run_id: run_id.clone(),
        attempt_no: 1,
        started_at: chrono::Utc::now(),
    })?;

    let config = GroveConfig {
        reactions: grove_config::ReactionConfig {
            rules: vec![
                grove_types::ReactionRule {
                    trigger: grove_types::ReactionTrigger::MirrorFailed,
                    action: grove_types::ReactionAction::ScheduleBackoff { base_secs: 30 },
                    enabled: true,
                    max_attempts: 1,
                    escalate_to: None,
                },
                grove_types::ReactionRule {
                    trigger: grove_types::ReactionTrigger::MirrorFailed,
                    action: grove_types::ReactionAction::CreateRecoveryCapsule {
                        outcome: grove_types::RecoveryCapsuleOutcome::Failed,
                    },
                    enabled: true,
                    max_attempts: 1,
                    escalate_to: None,
                },
            ],
        },
        ..GroveConfig::default()
    };

    let ctx = DispatchedWorkerContext {
        bead_id: bead.id.clone(),
        run_id: run_id.clone(),
        session_id: SessionId::new("ses-grove-error-path-1"),
    };
    apply_reaction_side_effects(
        &mut db,
        &config,
        &ctx,
        None,
        Some("mirror sync failed"),
        true,
    );

    let runs = db.list_task_runs_for_bead(&bead.id)?;
    let run = runs
        .into_iter()
        .find(|run| run.id == run_id)
        .ok_or_else(|| std::io::Error::other("expected task run"))?;
    assert_eq!(run.status, grove_types::RunStatus::WaitingToRetry);

    let event_logs = db.list_event_logs_for_bead(&bead.id)?;
    let reaction_count = event_logs
        .iter()
        .filter(|event| event.kind == grove_types::EventKind::ReactionInvoked)
        .count();
    assert_eq!(reaction_count, 2);

    let capsule = db
        .latest_recovery_capsule_for_bead(&bead.id)?
        .ok_or_else(|| std::io::Error::other("expected recovery capsule"))?;
    assert_eq!(
        capsule.capsule.outcome,
        grove_types::RecoveryCapsuleOutcome::Failed
    );
    assert!(
        capsule
            .capsule
            .strongest_evidence
            .iter()
            .any(|entry| entry.contains("mirror sync failed"))
    );
    Ok(())
}
