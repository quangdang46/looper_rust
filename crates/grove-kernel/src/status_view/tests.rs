
use super::*;
use grove_br::BrDependencySnapshot;
use grove_bv::{
    BvCommand, BvGraphHealth, BvProjectCounts, BvProjectHealth, BvQuickRef, BvRecommendation,
    BvTriageMeta, BvTriageOutput, BvVelocitySummary,
};
use grove_types::{
    BeadRef, CircuitBreakerState, CircuitState, HandoffRecord, MirrorStatus, RecoveryCapsule,
    RecoveryCapsuleOutcome, RunId, Timestamp,
};
use std::collections::{HashMap, HashSet};
use std::error::Error;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

#[test]
fn recovery_capsule_helpers_prefer_persisted_capsules() -> TestResult {
    let persisted = RecoveryCapsuleEvent {
        capsule: RecoveryCapsule::from_parts(
            RecoveryCapsuleOutcome::Interrupted,
            Some(FailureClass::Interrupted),
            Some("persisted detail"),
            None,
            Some("resume from checkpoint"),
            None,
            None,
            &[],
        )
        .ok_or("expected recovery capsule")?,
        source_event_id: 42,
        created_at: parse_ts("2026-03-20T06:45:00Z")?,
    };

    let checkpoint_capsule = recovery_capsule_for_checkpointed(None, None, Some(&persisted))
        .ok_or("expected checkpointed capsule")?;
    assert_eq!(
        checkpoint_capsule.outcome,
        RecoveryCapsuleOutcome::Interrupted
    );
    assert_eq!(
        checkpoint_capsule.recommended_next_step(),
        Some("resume from checkpoint")
    );

    let bead = sample_bead("grove-failed", "open", GroveBeadStatus::Failed)?;
    let failed_capsule = recovery_capsule_for_failed(&bead, None, None, Some(&persisted))
        .ok_or("expected failed capsule")?;
    assert_eq!(failed_capsule.summary, persisted.capsule.summary);
    Ok(())
}

#[test]
fn counts_br_and_grove_statuses_separately() -> TestResult {
    let beads = vec![
        sample_bead("grove-1", "open", GroveBeadStatus::Ready)?,
        sample_bead("grove-2", "open", GroveBeadStatus::Running)?,
        sample_bead("grove-3", "closed", GroveBeadStatus::Succeeded)?,
    ];

    let bead_counts = count_beads_statuses(&beads);
    let grove_counts = count_grove_statuses(&beads);

    assert_eq!(bead_counts.len(), 2);
    assert_eq!(bead_counts[0].status, "closed");
    assert_eq!(bead_counts[0].count, 1);
    assert_eq!(bead_counts[1].status, "open");
    assert_eq!(bead_counts[1].count, 2);

    assert_eq!(grove_counts.len(), 3);
    assert_eq!(grove_counts[0].status, "Ready");
    assert_eq!(grove_counts[1].status, "Running");
    assert_eq!(grove_counts[2].status, "Succeeded");
    Ok(())
}

#[test]
fn dispatch_explanation_uses_persisted_open_circuit_state() -> TestResult {
    let mut bead = sample_bead("grove-circuit", "open", GroveBeadStatus::Ready)?;
    bead.circuit_breaker_state = Some(CircuitBreakerState {
        state: CircuitState::Open,
        no_progress_count: 3,
        same_error_count: 0,
        permission_denial_count: 0,
        last_error_fingerprint: Some("same-error".to_owned()),
        opened_at: Some(parse_ts("2026-03-16T12:00:00Z")?),
    });
    let eligibility = crate::evaluate_dispatch_eligibility(
        &bead,
        &crate::DispatchEligibilityContext {
            ready_in_br: true,
            circuit_state: crate::circuit_state_for_bead(&bead),
            reservation_conflicts: Vec::new(),
            now: parse_ts("2026-03-16T12:00:00Z")?,
        },
    );

    let view = DispatchExplanationView::from_eligibility(&eligibility);

    assert!(!view.dispatchable_in_grove);
    assert_eq!(view.summary(), "circuit breaker is open");
    assert_eq!(view.local_suppression_reasons[0].code, "circuit_open");
    Ok(())
}

#[test]
fn dispatch_explanation_summarizes_local_suppressions() -> TestResult {
    let bead = sample_bead("grove-4", "open", GroveBeadStatus::WaitingToRetry)?;
    let eligibility = crate::evaluate_dispatch_eligibility(
        &bead,
        &crate::DispatchEligibilityContext {
            ready_in_br: true,
            circuit_state: CircuitState::Closed,
            reservation_conflicts: Vec::new(),
            now: parse_ts("2026-03-16T12:00:00Z")?,
        },
    );

    let view = DispatchExplanationView::from_eligibility(&eligibility);

    assert!(!view.dispatchable_in_grove);
    assert_eq!(view.summary(), "retry backoff still pending");
    assert_eq!(
        view.local_suppression_reasons[0].code,
        "retry_backoff_pending"
    );
    Ok(())
}

#[test]
fn suppression_reason_carries_reservation_conflict_details() {
    let reason = LocalSuppressionReason::ReservationConflict {
        conflict: ReservationConflict {
            requested_by_bead: BeadId::new("grove-req"),
            conflicting_bead: BeadId::new("grove-held"),
            requested_pattern: "src/**".to_owned(),
            held_pattern: "src/lib.rs".to_owned(),
            conflicting_run_id: Some(RunId::new("run-7")),
        },
    };

    let view = SuppressionReasonView::from_reason(&reason);

    assert_eq!(view.code, "reservation_conflict");
    assert_eq!(
        view.summary,
        "reservation conflict between grove-req (src/**) and grove-held (src/lib.rs)"
    );
    assert_eq!(
        view.conflict
            .as_ref()
            .map(|conflict| conflict.held_pattern.as_str()),
        Some("src/lib.rs")
    );
}

#[test]
fn ready_queue_orders_by_score_then_bead_id() -> TestResult {
    let mut p1_with_bonus =
        sample_bead_with_priority("grove-a", "open", GroveBeadStatus::Ready, BeadPriority::P1)?;
    p1_with_bonus.bead.title = "priority bonus".to_owned();

    let mut p0 =
        sample_bead_with_priority("grove-b", "open", GroveBeadStatus::Ready, BeadPriority::P0)?;
    p0.bead.title = "top priority".to_owned();

    let mut p1_plain =
        sample_bead_with_priority("grove-c", "open", GroveBeadStatus::Ready, BeadPriority::P1)?;
    p1_plain.bead.title = "plain p1".to_owned();

    let ready_ids = HashSet::from([
        p1_with_bonus.bead.id.clone(),
        p0.bead.id.clone(),
        p1_plain.bead.id.clone(),
    ]);
    let dependency_map = HashMap::from([(
        p1_with_bonus.bead.id.clone(),
        dependency_snapshot(&p1_with_bonus.bead.id, &["grove-child"]),
    )]);

    let queue = build_ready_queue(
        &[p1_with_bonus, p0, p1_plain],
        &ready_ids,
        &dependency_map,
        &[],
        &HashMap::new(),
        &GroveConfig::default(),
        None,
    );

    let ordered_ids = queue
        .iter()
        .map(|entry| entry.bead_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ordered_ids, vec!["grove-b", "grove-a", "grove-c"]);

    let p0_entry = &queue[0];
    assert!(p0_entry.score.is_some_and(|score| score >= 100.0));
    assert!(p0_entry.why.iter().any(|item| item == "P0 priority"));
    assert!(
        p0_entry
            .why
            .iter()
            .any(|item| item == "no reservation conflicts")
    );
    assert!(
        p0_entry
            .score_breakdown
            .iter()
            .any(|component| component.label == "ready_age")
    );

    let bonus_entry = &queue[1];
    assert!(bonus_entry.score.is_some_and(|score| score >= 95.0));
    assert!(
        bonus_entry
            .score_breakdown
            .iter()
            .any(|component| component.label == "critical_path" && component.value == 20.0)
    );
    assert!(
        bonus_entry
            .why
            .iter()
            .any(|item| item == "1 downstream bead")
    );

    let tied_queue = build_ready_queue(
        &[
            sample_bead_with_priority("grove-z", "open", GroveBeadStatus::Ready, BeadPriority::P1)?,
            sample_bead_with_priority("grove-y", "open", GroveBeadStatus::Ready, BeadPriority::P1)?,
        ],
        &HashSet::from([BeadId::new("grove-z"), BeadId::new("grove-y")]),
        &HashMap::new(),
        &[],
        &HashMap::new(),
        &GroveConfig::default(),
        None,
    );
    let tied_ids = tied_queue
        .iter()
        .map(|entry| entry.bead_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(tied_ids, vec!["grove-y", "grove-z"]);

    Ok(())
}

#[test]
fn ready_queue_keeps_ready_but_suppressed_entries_with_conflict_penalty() -> TestResult {
    let clean = sample_bead_with_priority(
        "grove-clean",
        "open",
        GroveBeadStatus::Ready,
        BeadPriority::P1,
    )?;
    let conflicted = sample_bead_with_priority(
        "grove-conflicted",
        "open",
        GroveBeadStatus::Ready,
        BeadPriority::P1,
    )?;

    let ready_ids = HashSet::from([clean.bead.id.clone(), conflicted.bead.id.clone()]);
    let conflict = ReservationConflict {
        requested_by_bead: conflicted.bead.id.clone(),
        conflicting_bead: BeadId::new("grove-held"),
        requested_pattern: "crates/grove-kernel/src/status_view.rs".to_owned(),
        held_pattern: "crates/grove-kernel/src/*".to_owned(),
        conflicting_run_id: Some(RunId::new("run-held")),
    };

    let queue = build_ready_queue(
        &[clean, conflicted],
        &ready_ids,
        &HashMap::new(),
        &[conflict],
        &HashMap::new(),
        &GroveConfig::default(),
        None,
    );

    assert_eq!(queue.len(), 2);

    let clean_entry = queue
        .iter()
        .find(|entry| entry.bead_id.as_str() == "grove-clean")
        .expect("clean ready bead should stay in queue");
    assert!(clean_entry.dispatch.dispatchable_in_grove);
    assert!(clean_entry.score.is_some_and(|score| score >= 75.0));
    assert!(
        clean_entry
            .score_breakdown
            .iter()
            .all(|component| component.label != "reservation_conflict_penalty")
    );

    let conflicted_entry = queue
        .iter()
        .find(|entry| entry.bead_id.as_str() == "grove-conflicted")
        .expect("conflicted ready bead should stay in queue");
    assert!(!conflicted_entry.dispatch.dispatchable_in_grove);
    assert_eq!(
        conflicted_entry.dispatch.summary(),
        "reservation conflict between grove-conflicted (crates/grove-kernel/src/status_view.rs) and grove-held (crates/grove-kernel/src/*)"
    );
    assert!(
        conflicted_entry
            .dispatch
            .local_suppression_reasons
            .iter()
            .any(|reason| reason.code == "reservation_conflict")
    );
    assert!(conflicted_entry.score_breakdown.iter().any(|component| {
        component.label == "reservation_conflict_penalty"
            && component.value == -1000.0
            && component.note.as_deref() == Some("1 active conflict(s)")
    }));
    assert!(
        conflicted_entry
            .score_breakdown
            .iter()
            .any(|component| component.label == "ready_age")
    );
    assert!(
        conflicted_entry
            .why
            .iter()
            .any(|item| item == "1 reservation conflict(s)")
    );

    Ok(())
}

#[test]
fn checkpointed_beads_hide_stale_checkpoint_from_older_run() -> TestResult {
    use grove_db::Database;
    use tempfile::tempdir;

    let dir = tempdir()?;
    let db_path = camino::Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
        .map_err(|_| std::io::Error::other("temp path was not valid UTF-8"))?;
    let mut db = Database::open(&db_path)?;
    db.migrate()?;

    db.connection().execute(
            "INSERT INTO bead_cache(\
                bead_id, title, description, priority, issue_type, status, assignee,\
                labels_json, parent_ids_json, dependency_ids_json, dependent_ids_json, raw_json, synced_at\
            ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, NULL, '[]', '[]', '[]', '[]', '{}', ?6)",
            rusqlite::params![
                "grove-child",
                "Child bead",
                1,
                "task",
                "open",
                "2026-03-16T09:00:00Z",
            ],
        )?;
    db.connection().execute(
            "INSERT INTO task_runs(\
                id, bead_id, attempt_no, status, failure_class, failure_detail, started_at, ended_at, session_count, checkpoint_count, last_checkpoint_id\
            ) VALUES (?1, ?2, ?3, ?4, NULL, NULL, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                "run-old",
                "grove-child",
                1,
                "Checkpointed",
                "2026-03-16T11:00:00Z",
                "2026-03-16T11:10:00Z",
                1,
                1,
                "chk-old",
            ],
        )?;
    db.connection().execute(
            "INSERT INTO task_runs(\
                id, bead_id, attempt_no, status, failure_class, failure_detail, started_at, ended_at, session_count, checkpoint_count, last_checkpoint_id\
            ) VALUES (?1, ?2, ?3, ?4, NULL, NULL, ?5, NULL, ?6, ?7, ?8)",
            rusqlite::params![
                "run-new",
                "grove-child",
                2,
                "Checkpointed",
                "2026-03-16T12:00:00Z",
                1,
                0,
                Option::<String>::None,
            ],
        )?;
    db.connection().execute(
            "INSERT INTO bead_runtime(\
                bead_id, grove_status, declared_paths_json, metadata_json, last_run_id, retry_after,\
                last_failure_class, last_failure_detail, runtime_updated_at\
            ) VALUES (?1, ?2, '[]', '{}', ?3, NULL, NULL, NULL, ?4)",
            rusqlite::params![
                "grove-child",
                "Checkpointed",
                "run-new",
                "2026-03-16T12:05:00Z",
            ],
        )?;
    db.connection().execute(
            "INSERT INTO claude_sessions(\
                id, run_id, external_session_id, ordinal_in_run, status, started_at, ended_at, prompt_id, prompt_manifest_path, prompt_bytes, estimated_input_tokens, estimated_output_tokens, exit_code, stop_reason, transcript_path\
            ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, NULL, NULL, ?7, ?8, ?9, ?10, ?11, ?12)",
            rusqlite::params![
                "ses-old",
                "run-old",
                1,
                "Checkpointed",
                "2026-03-16T11:00:00Z",
                "2026-03-16T11:09:00Z",
                100,
                25,
                35,
                0,
                "Checkpoint",
                ".grove/transcripts/grove-child/ses-old.jsonl",
            ],
        )?;
    db.connection().execute(
            "INSERT INTO checkpoints(\
                id, bead_id, run_id, session_id, progress, next_step, payload_json, saved_at, resume_generation\
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                "chk-old",
                "grove-child",
                "run-old",
                "ses-old",
                "halfway there",
                "resume older run",
                "{\"progress\":\"halfway there\",\"next_step\":\"resume older run\",\"context\":{},\"open_questions\":[],\"claimed_paths\":[\"crates/grove-kernel/src/status_view.rs\"],\"confidence\":null}",
                "2026-03-16T11:09:00Z",
                1,
            ],
        )?;

    let bead = GroveBeadRecord {
        bead: BeadRef {
            id: BeadId::new("grove-child"),
            title: "Child bead".to_owned(),
            description: None,
            priority: BeadPriority::P1,
            issue_type: "task".to_owned(),
            br_status: "open".to_owned(),
            assignee: None,
            labels: Vec::new(),
            created_at: parse_ts("2026-03-16T10:00:00Z")?,
            updated_at: parse_ts("2026-03-16T12:05:00Z")?,
        },
        grove_status: GroveBeadStatus::Checkpointed,
        declared_paths: Vec::new(),
        metadata: Default::default(),
        last_run_id: Some(RunId::new("run-new")),
        retry_after: None,
        last_failure_class: None,
        last_failure_detail: None,
        circuit_breaker_state: None,
        synced_at: parse_ts("2026-03-16T12:05:00Z")?,
        runtime_updated_at: parse_ts("2026-03-16T12:05:00Z")?,
    };

    let checkpointed = build_checkpointed_beads(&[bead], &db, &HashMap::new())?;
    assert_eq!(checkpointed.len(), 1);
    assert_eq!(
        checkpointed[0].run_id.as_ref().map(RunId::as_str),
        Some("run-new")
    );
    assert_eq!(checkpointed[0].checkpoint_id, None);
    assert_eq!(checkpointed[0].progress, None);
    assert_eq!(checkpointed[0].next_step, None);
    assert!(checkpointed[0].claimed_paths.is_empty());
    assert_eq!(checkpointed[0].saved_at, None);
    Ok(())
}

#[test]
fn reservation_conflict_detection_handles_common_glob_and_file_cases() {
    let reservations = vec![
        ReservationRecord {
            id: 1,
            bead_id: BeadId::new("grove-file"),
            run_id: Some(RunId::new("run-file")),
            path_pattern: "crates/grove-db/src/lib.rs".to_owned(),
            mode: ReservationMode::Exclusive,
            reason: None,
            expires_at: parse_ts("2099-03-16T12:30:00Z").expect("valid timestamp"),
            released_at: None,
        },
        ReservationRecord {
            id: 2,
            bead_id: BeadId::new("grove-glob"),
            run_id: None,
            path_pattern: "crates/grove-db/src/*.rs".to_owned(),
            mode: ReservationMode::Exclusive,
            reason: None,
            expires_at: parse_ts("2099-03-16T12:30:00Z").expect("valid timestamp"),
            released_at: None,
        },
        ReservationRecord {
            id: 3,
            bead_id: BeadId::new("grove-tree"),
            run_id: Some(RunId::new("run-tree")),
            path_pattern: "crates/grove-db/src/**".to_owned(),
            mode: ReservationMode::Exclusive,
            reason: None,
            expires_at: parse_ts("2099-03-16T12:30:00Z").expect("valid timestamp"),
            released_at: None,
        },
        ReservationRecord {
            id: 4,
            bead_id: BeadId::new("grove-other"),
            run_id: Some(RunId::new("run-other")),
            path_pattern: "crates/grove-kernel/src/lib.rs".to_owned(),
            mode: ReservationMode::Exclusive,
            reason: None,
            expires_at: parse_ts("2099-03-16T12:30:00Z").expect("valid timestamp"),
            released_at: None,
        },
    ];

    let conflicts = find_reservation_conflicts(&reservations);
    assert_eq!(conflicts.len(), 6);

    let file_conflict = conflicts_for_bead(&BeadId::new("grove-file"), &conflicts);
    assert_eq!(file_conflict.len(), 2);
    assert!(file_conflict.iter().any(|conflict| {
        conflict.conflicting_bead.as_str() == "grove-glob"
            && conflict.conflicting_run_id.is_none()
            && conflict.held_pattern == "crates/grove-db/src/*.rs"
    }));
    assert!(file_conflict.iter().any(|conflict| {
        conflict.conflicting_bead.as_str() == "grove-tree"
            && conflict.conflicting_run_id.as_ref().map(RunId::as_str) == Some("run-tree")
            && conflict.held_pattern == "crates/grove-db/src/**"
    }));

    let glob_conflict = conflicts_for_bead(&BeadId::new("grove-glob"), &conflicts);
    assert_eq!(glob_conflict.len(), 2);
    assert!(glob_conflict.iter().any(|conflict| {
        conflict.conflicting_bead.as_str() == "grove-file"
            && conflict.conflicting_run_id.as_ref().map(RunId::as_str) == Some("run-file")
            && conflict.held_pattern == "crates/grove-db/src/lib.rs"
    }));
    assert!(glob_conflict.iter().any(|conflict| {
        conflict.conflicting_bead.as_str() == "grove-tree"
            && conflict.conflicting_run_id.as_ref().map(RunId::as_str) == Some("run-tree")
            && conflict.held_pattern == "crates/grove-db/src/**"
    }));

    let other_conflict = conflicts_for_bead(&BeadId::new("grove-other"), &conflicts);
    assert!(other_conflict.is_empty());
}

#[test]
fn ready_queue_blends_bv_triage_and_ready_age_bonus() -> TestResult {
    let bead =
        sample_bead_with_priority("grove-bv", "open", GroveBeadStatus::Ready, BeadPriority::P1)?;
    let ready_ids = HashSet::from([bead.bead.id.clone()]);
    let triage =
        sample_triage_output(&bead.bead.id, 0.75, &["critical path bead", "top pagerank"])?;

    let queue = build_ready_queue(
        &[bead],
        &ready_ids,
        &HashMap::new(),
        &[],
        &HashMap::new(),
        &GroveConfig::default(),
        Some(&triage),
    );

    let entry = queue.first().ok_or("expected ready entry")?;
    assert_eq!(entry.bv_score, Some(0.75));
    assert!(entry.ready_minutes.is_some());
    assert!(
        entry
            .score_breakdown
            .iter()
            .any(|component| component.label == "bv_triage" && component.value == 0.75)
    );
    assert!(
        entry
            .score_breakdown
            .iter()
            .any(|component| component.label == "ready_age")
    );
    assert!(
        entry
            .why
            .iter()
            .any(|item| item.contains("bv triage 0.75: critical path bead, top pagerank"))
    );
    Ok(())
}

fn sample_bead(
    id: &str,
    br_status: &str,
    grove_status: GroveBeadStatus,
) -> TestResult<GroveBeadRecord> {
    sample_bead_with_priority(id, br_status, grove_status, BeadPriority::P1)
}

fn sample_bead_with_priority(
    id: &str,
    br_status: &str,
    grove_status: GroveBeadStatus,
    priority: BeadPriority,
) -> TestResult<GroveBeadRecord> {
    let created_at = parse_ts("2026-03-16T10:00:00Z")?;
    let updated_at = parse_ts("2026-03-16T11:00:00Z")?;
    let retry_after = match grove_status {
        GroveBeadStatus::WaitingToRetry => Some(parse_ts("2026-03-16T12:30:00Z")?),
        _ => None,
    };

    Ok(GroveBeadRecord {
        bead: BeadRef {
            id: BeadId::new(id),
            title: format!("title-{id}"),
            description: None,
            priority,
            issue_type: "task".to_owned(),
            br_status: br_status.to_owned(),
            assignee: None,
            labels: Vec::new(),
            created_at,
            updated_at,
        },
        grove_status,
        declared_paths: Vec::new(),
        metadata: Default::default(),
        last_run_id: None,
        retry_after,
        last_failure_class: None,
        last_failure_detail: None,
        circuit_breaker_state: None,
        synced_at: updated_at,
        runtime_updated_at: updated_at,
    })
}

fn dependency_snapshot(bead_id: &BeadId, blocks: &[&str]) -> BrDependencySnapshot {
    BrDependencySnapshot {
        bead_id: bead_id.clone(),
        blocked_by: Vec::new(),
        blocks: blocks.iter().map(|id| BeadId::new(*id)).collect(),
        rows: Vec::new(),
    }
}

fn sample_triage_output(
    bead_id: &BeadId,
    score: f64,
    reasons: &[&str],
) -> TestResult<BvTriageOutput> {
    Ok(BvTriageOutput {
        generated_at: parse_ts("2026-03-16T12:00:00Z")?,
        data_hash: "hash".to_owned(),
        meta: BvTriageMeta {
            version: "test".to_owned(),
            generated_at: parse_ts("2026-03-16T12:00:00Z")?,
            phase2_ready: true,
            issue_count: 1,
            compute_time_ms: 1,
        },
        quick_ref: BvQuickRef {
            open_count: 1,
            actionable_count: 1,
            blocked_count: 0,
            in_progress_count: 0,
            top_picks: Vec::new(),
        },
        recommendations: vec![BvRecommendation {
            id: bead_id.clone(),
            title: "triaged".to_owned(),
            issue_type: "task".to_owned(),
            status: "open".to_owned(),
            priority: BeadPriority::P1,
            labels: Vec::new(),
            score,
            breakdown_json: serde_json::json!({}),
            action: None,
            reasons: reasons.iter().map(|reason| (*reason).to_owned()).collect(),
            unblocks: Vec::new(),
            blocked_by: Vec::new(),
            page_rank: Some(0.5),
            betweenness: Some(0.2),
        }],
        quick_wins: Vec::new(),
        blockers_to_clear: Vec::new(),
        project_health: BvProjectHealth {
            counts: BvProjectCounts {
                total: 1,
                open: 1,
                closed: 0,
                blocked: 0,
                actionable: 1,
                by_status: HashMap::new(),
                by_type: HashMap::new(),
                by_priority: HashMap::new(),
            },
            graph: BvGraphHealth {
                node_count: 1,
                edge_count: 0,
                density: None,
                has_cycles: false,
                phase2_ready: true,
            },
            velocity: BvVelocitySummary {
                closed_last_7_days: 0,
                closed_last_30_days: 0,
                avg_days_to_close: None,
                weekly: Vec::new(),
            },
        },
        commands: vec![BvCommand {
            label: "next".to_owned(),
            command: "bv --robot-next".to_owned(),
        }],
        usage_hints: Vec::new(),
    })
}

fn parse_ts(value: &str) -> TestResult<Timestamp> {
    Ok(value.parse()?)
}

#[test]
fn latest_mirror_pending_uses_outbox_rows() -> TestResult {
    let dir = tempfile::tempdir()?;
    let db_path = camino::Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
        .map_err(|_| std::io::Error::other("temp path was not valid UTF-8"))?;
    let mut db = Database::open(&db_path)?;
    db.migrate()?;

    let bead_id = BeadId::new("grove-1j9.7.6");
    let run_id = RunId::new("run-1");
    db.connection().execute(
            "INSERT INTO bead_cache(\
                bead_id, title, description, priority, issue_type, status, assignee,\
                labels_json, parent_ids_json, dependency_ids_json, dependent_ids_json, raw_json, synced_at\
            ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, NULL, '[]', '[]', '[]', '[]', '{}', ?6)",
            rusqlite::params![
                bead_id.as_str(),
                "Mirror bead",
                1,
                "task",
                "open",
                "2026-03-20T05:55:00Z",
            ],
        )?;
    db.connection().execute(
            "INSERT INTO task_runs(\
                id, bead_id, attempt_no, status, failure_class, failure_detail, started_at, ended_at, session_count, checkpoint_count, last_checkpoint_id\
            ) VALUES (?1, ?2, ?3, ?4, NULL, NULL, ?5, NULL, ?6, ?7, ?8)",
            rusqlite::params![
                run_id.as_str(),
                bead_id.as_str(),
                1,
                "Succeeded",
                "2026-03-20T06:00:00Z",
                1,
                0,
                Option::<String>::None,
            ],
        )?;
    let handoff = HandoffRecord {
        bead_id: bead_id.clone(),
        run_id: run_id.clone(),
        summary: "done locally".to_owned(),
        artifacts: vec!["crates/grove-kernel/src/status_view.rs".to_owned()],
        lessons: Vec::new(),
        decisions: Vec::new(),
        warnings: Vec::new(),
        completed_at: parse_ts("2026-03-20T06:00:00Z")?,
    };

    db.enqueue_mirror_outbox(&bead_id, &run_id, &handoff, false)?;

    let pending = latest_mirror_pending_for_bead(&bead_id, &db)?
        .ok_or_else(|| std::io::Error::other("expected pending mirror view"))?;
    assert_eq!(pending.run_id.as_ref(), Some(&run_id));
    assert_eq!(pending.pending_actions, vec!["comment".to_owned()]);
    assert_eq!(pending.last_attempt_at, None);
    assert_eq!(pending.last_error, None);
    Ok(())
}

#[test]
fn latest_mirror_pending_includes_close_and_failure_details() -> TestResult {
    let dir = tempfile::tempdir()?;
    let db_path = camino::Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
        .map_err(|_| std::io::Error::other("temp path was not valid UTF-8"))?;
    let mut db = Database::open(&db_path)?;
    db.migrate()?;

    let bead_id = BeadId::new("grove-1j9.7.6");
    let run_id = RunId::new("run-2");
    db.connection().execute(
            "INSERT INTO bead_cache(\
                bead_id, title, description, priority, issue_type, status, assignee,\
                labels_json, parent_ids_json, dependency_ids_json, dependent_ids_json, raw_json, synced_at\
            ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, NULL, '[]', '[]', '[]', '[]', '{}', ?6)",
            rusqlite::params![
                bead_id.as_str(),
                "Mirror bead",
                1,
                "task",
                "open",
                "2026-03-20T06:00:00Z",
            ],
        )?;
    db.connection().execute(
            "INSERT INTO task_runs(\
                id, bead_id, attempt_no, status, failure_class, failure_detail, started_at, ended_at, session_count, checkpoint_count, last_checkpoint_id\
            ) VALUES (?1, ?2, ?3, ?4, NULL, NULL, ?5, NULL, ?6, ?7, ?8)",
            rusqlite::params![
                run_id.as_str(),
                bead_id.as_str(),
                1,
                "Succeeded",
                "2026-03-20T06:05:00Z",
                1,
                0,
                Option::<String>::None,
            ],
        )?;
    let handoff = HandoffRecord {
        bead_id: bead_id.clone(),
        run_id: run_id.clone(),
        summary: "done locally".to_owned(),
        artifacts: Vec::new(),
        lessons: Vec::new(),
        decisions: Vec::new(),
        warnings: Vec::new(),
        completed_at: parse_ts("2026-03-20T06:05:00Z")?,
    };

    let operation = db.enqueue_mirror_outbox(&bead_id, &run_id, &handoff, true)?;
    let retry_after = chrono::Utc::now();
    db.record_mirror_failure(&operation.id, &run_id, "network hiccup", Some(&retry_after))?;

    let pending = latest_mirror_pending_for_bead(&bead_id, &db)?
        .ok_or_else(|| std::io::Error::other("expected pending mirror view"))?;
    assert_eq!(
        pending.pending_actions,
        vec!["comment".to_owned(), "close".to_owned()]
    );
    assert_eq!(pending.last_error.as_deref(), Some("network hiccup"));
    assert!(pending.last_attempt_at.is_some());
    Ok(())
}

#[test]
fn latest_mirror_pending_ignores_succeeded_operations() -> TestResult {
    let dir = tempfile::tempdir()?;
    let db_path = camino::Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
        .map_err(|_| std::io::Error::other("temp path was not valid UTF-8"))?;
    let mut db = Database::open(&db_path)?;
    db.migrate()?;

    let bead_id = BeadId::new("grove-1j9.7.6");
    let run_id = RunId::new("run-3");
    db.connection().execute(
            "INSERT INTO bead_cache(\
                bead_id, title, description, priority, issue_type, status, assignee,\
                labels_json, parent_ids_json, dependency_ids_json, dependent_ids_json, raw_json, synced_at\
            ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, NULL, '[]', '[]', '[]', '[]', '{}', ?6)",
            rusqlite::params![
                bead_id.as_str(),
                "Mirror bead",
                1,
                "task",
                "open",
                "2026-03-20T06:05:00Z",
            ],
        )?;
    db.connection().execute(
            "INSERT INTO task_runs(\
                id, bead_id, attempt_no, status, failure_class, failure_detail, started_at, ended_at, session_count, checkpoint_count, last_checkpoint_id\
            ) VALUES (?1, ?2, ?3, ?4, NULL, NULL, ?5, NULL, ?6, ?7, ?8)",
            rusqlite::params![
                run_id.as_str(),
                bead_id.as_str(),
                1,
                "Succeeded",
                "2026-03-20T06:10:00Z",
                1,
                0,
                Option::<String>::None,
            ],
        )?;
    let handoff = HandoffRecord {
        bead_id: bead_id.clone(),
        run_id: run_id.clone(),
        summary: "done locally".to_owned(),
        artifacts: Vec::new(),
        lessons: Vec::new(),
        decisions: Vec::new(),
        warnings: Vec::new(),
        completed_at: parse_ts("2026-03-20T06:10:00Z")?,
    };

    let operation = db.enqueue_mirror_outbox(&bead_id, &run_id, &handoff, true)?;
    db.record_mirror_success(&operation.id, &run_id)?;

    assert!(latest_mirror_pending_for_bead(&bead_id, &db)?.is_none());
    Ok(())
}

#[test]
fn pending_actions_follow_outbox_close_flag() -> TestResult {
    let timestamp = parse_ts("2026-03-20T06:15:00Z")?;
    let base = grove_types::MirrorOutboxRecord {
        id: "mirror-1".to_owned(),
        bead_id: BeadId::new("grove-1"),
        run_id: RunId::new("run-1"),
        handoff: HandoffRecord {
            bead_id: BeadId::new("grove-1"),
            run_id: RunId::new("run-1"),
            summary: "summary".to_owned(),
            artifacts: Vec::new(),
            lessons: Vec::new(),
            decisions: Vec::new(),
            warnings: Vec::new(),
            completed_at: timestamp,
        },
        close_bead: false,
        mirror_status: MirrorStatus::Pending,
        attempt_count: 0,
        last_attempt_at: None,
        next_retry_after: None,
        last_error: None,
        created_at: timestamp,
        updated_at: timestamp,
    };

    assert_eq!(
        pending_actions_for_operation(&base),
        vec!["comment".to_owned()]
    );

    let close = grove_types::MirrorOutboxRecord {
        close_bead: true,
        ..base
    };
    assert_eq!(
        pending_actions_for_operation(&close),
        vec!["comment".to_owned(), "close".to_owned()]
    );
    Ok(())
}
