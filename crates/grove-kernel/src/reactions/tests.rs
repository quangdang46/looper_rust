
use super::*;
use grove_br::BeadCacheStore;
use grove_types::default_reactions;
use tempfile::tempdir;

fn make_ctx(status: RunStatus, failures: u32, tier: EscalationTier) -> TriggerContext {
    TriggerContext {
        bead_id: BeadId::new("test-1"),
        run_id: RunId::new("run-1"),
        run_status: status,
        activity: AgentActivity::Active,
        failure_class: Some(FailureClass::NoProgress),
        failure_detail: None,
        escalation_tier: tier,
        consecutive_failures: failures,
        circuit_state: grove_types::CircuitState::Closed,
        context_pressure_pct: None,
        mirror_failed: false,
    }
}

#[test]
fn no_reactions_on_success() {
    let rules = default_reactions();
    let ctx = make_ctx(RunStatus::Succeeded, 0, EscalationTier::FirstAttempt);
    // We can't call evaluate_reactions without a real DB, but we can
    // test trigger matching individually
    for rule in &rules {
        assert!(
            !trigger_matches(&rule.trigger, &ctx),
            "No trigger should fire on success"
        );
    }
}

#[test]
fn no_progress_triggers_after_threshold() {
    let rules = default_reactions();
    let ctx = make_ctx(RunStatus::Failed, 3, EscalationTier::FirstAttempt);
    let no_progress_rule = rules
        .iter()
        .find(|r| matches!(r.trigger, ReactionTrigger::NoProgress { .. }))
        .unwrap();
    assert!(trigger_matches(&no_progress_rule.trigger, &ctx));
}

#[test]
fn retry_budget_exhausted_at_give_up() {
    let ctx = make_ctx(RunStatus::Failed, 5, EscalationTier::GiveUp);
    assert!(trigger_matches(
        &ReactionTrigger::RetryBudgetExhausted,
        &ctx
    ));
}

#[test]
fn context_pressure_triggers_above_85() {
    let mut ctx = make_ctx(RunStatus::Active, 0, EscalationTier::FirstAttempt);
    ctx.context_pressure_pct = Some(0.90);
    assert!(trigger_matches(&ReactionTrigger::ContextPressureHigh, &ctx));
}

#[test]
fn idle_trigger_matches_idle_activity() {
    let mut ctx = make_ctx(RunStatus::WaitingToRetry, 1, EscalationTier::SecondAttempt);
    ctx.activity = AgentActivity::Idle;
    assert!(trigger_matches(
        &ReactionTrigger::AgentIdle { duration_secs: 300 },
        &ctx
    ));
}

#[test]
fn action_escalates_at_terminal_tier() {
    let action = ReactionAction::InjectRescue {
        prompt: "test".into(),
    };
    let (outcome, _, _) = apply_action(&action, None, &EscalationTier::GiveUp);
    assert_eq!(outcome, ReactionOutcome::Escalated);
}

#[test]
fn mirror_failed_trigger_matches_explicit_context_flag() {
    let mut ctx = make_ctx(RunStatus::Failed, 1, EscalationTier::SecondAttempt);
    ctx.failure_class = Some(FailureClass::BrMirrorFailed);
    ctx.mirror_failed = true;
    assert!(trigger_matches(&ReactionTrigger::MirrorFailed, &ctx));
}

#[test]
fn circuit_open_trigger_matches_failure_class() {
    let mut ctx = make_ctx(RunStatus::Failed, 1, EscalationTier::SecondAttempt);
    ctx.failure_class = Some(FailureClass::CircuitOpen);
    assert!(trigger_matches(&ReactionTrigger::CircuitOpen, &ctx));
}

#[test]
fn context_pressure_triggers_at_threshold() {
    let mut ctx = make_ctx(RunStatus::Active, 0, EscalationTier::FirstAttempt);
    ctx.context_pressure_pct = Some(0.85);
    assert!(trigger_matches(&ReactionTrigger::ContextPressureHigh, &ctx));
}

#[test]
fn load_reaction_rules_prefers_configured_rules() {
    let mut config = grove_config::GroveConfig::default();
    config.reactions.rules = vec![ReactionRule {
        trigger: ReactionTrigger::MirrorFailed,
        action: ReactionAction::EnqueueMirrorRetry,
        enabled: true,
        max_attempts: 9,
        escalate_to: None,
    }];
    let loaded = load_reaction_rules(&config);
    assert_eq!(loaded.len(), 1);
    assert!(matches!(loaded[0].trigger, ReactionTrigger::MirrorFailed));
    assert_eq!(loaded[0].max_attempts, 9);
}

#[test]
fn infer_blocked_activity_from_permission_denial() {
    let outcome = SessionOutcome {
        session: grove_types::ClaudeSessionRecord {
            id: grove_types::SessionId::new("ses-1"),
            run_id: RunId::new("run-1"),
            provider: grove_types::RuntimeProvider::Claude,
            external_session_id: None,
            ordinal_in_run: 1,
            status: grove_types::SessionStatus::PermissionDenied,
            started_at: "2026-03-21T00:00:00Z".parse().unwrap(),
            ended_at: Some("2026-03-21T00:01:00Z".parse().unwrap()),
            prompt_id: None,
            prompt_manifest_path: None,
            prompt_bytes: 0,
            estimated_input_tokens: 0,
            estimated_output_tokens: 0,
            exit_code: Some(1),
            stop_reason: Some(grove_types::StopReason::PermissionDenied),
            transcript_path: "trace.jsonl".into(),
        },
        protocol_events: Vec::new(),
        analysis: grove_types::IterationAnalysis {
            permission_denials: 1,
            ..Default::default()
        },
        terminal_class: grove_types::SessionTerminalClass::PermissionDenied,
        context_pressure_pct: None,
        context_pressure_level: grove_types::ContextPressureLevel::Ok,
        stdout_tail: Vec::new(),
        stderr_tail: vec!["permission denied".into()],
    };

    assert_eq!(
        infer_agent_activity(&outcome, RunStatus::Failed),
        AgentActivity::Blocked
    );
}

#[test]
fn evaluate_reactions_stops_after_max_attempts() {
    let dir = tempdir().unwrap();
    let db_path = camino::Utf8PathBuf::from_path_buf(dir.path().join("grove.db")).unwrap();
    let mut db = grove_db::Database::open(&db_path).unwrap();
    db.migrate().unwrap();

    let bead_id = BeadId::new("grove-reaction-cap");
    let run_id = RunId::new("run-cap-1");
    db.upsert_bead_cache(&grove_br::BrIssueSummary {
        id: bead_id.clone(),
        title: "reaction cap".into(),
        description: None,
        priority: grove_types::BeadPriority::P1,
        issue_type: "task".into(),
        status: "open".into(),
        assignee: None,
        labels: Vec::new(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        blocked_by: Vec::new(),
        blocks: Vec::new(),
        raw_json: serde_json::json!({}),
    })
    .unwrap();
    db.record_run_started(grove_db::RunStartInput {
        bead_id: bead_id.clone(),
        run_id: run_id.clone(),
        attempt_no: 1,
        started_at: chrono::Utc::now(),
        escalation_tier: grove_types::EscalationTier::FirstAttempt,
    })
    .unwrap();

    let trigger = ReactionTrigger::MirrorFailed;
    let prior = ReactionRecord {
        id: "rxn-prior".into(),
        trigger: trigger.clone(),
        action: ReactionAction::EnqueueMirrorRetry,
        outcome: ReactionOutcome::Applied,
        escalated_to: None,
        recovery_capsule: None,
        context: None,
        invoked_at: chrono::Utc::now(),
        success: true,
        error: None,
        run_id: Some(run_id.clone()),
    };
    db.write_event_log(
        grove_types::EventKind::ReactionInvoked,
        Some(&bead_id),
        Some(&run_id),
        None,
        &serde_json::to_value(&prior).unwrap(),
        &chrono::Utc::now(),
    )
    .unwrap();

    let ctx = TriggerContext {
        bead_id,
        run_id,
        run_status: RunStatus::Failed,
        activity: AgentActivity::Blocked,
        failure_class: Some(FailureClass::BrMirrorFailed),
        failure_detail: Some("mirror failed".into()),
        escalation_tier: EscalationTier::SecondAttempt,
        consecutive_failures: 1,
        circuit_state: grove_types::CircuitState::Closed,
        context_pressure_pct: None,
        mirror_failed: true,
    };
    let rules = vec![ReactionRule {
        trigger,
        action: ReactionAction::EnqueueMirrorRetry,
        enabled: true,
        max_attempts: 1,
        escalate_to: None,
    }];

    let result = evaluate_reactions(&mut db, &ctx, &rules);
    assert!(result.records.is_empty());
    assert_eq!(result.new_tier, EscalationTier::SecondAttempt);
}
