
use super::*;
use crate::ExitPolicy;
use grove_types::{
    ClaudeSessionRecord, ContextPressureLevel, ProtocolEvent, ProtocolState, RunId, SessionId,
    SessionOutcome, SessionStatus, SessionTerminalClass, StopReason,
};

fn base_context<'a>(
    protocol_state: &'a ProtocolState,
    stdout_lines: &'a [String],
    stderr_lines: &'a [String],
) -> SessionAnalysisContext<'a> {
    SessionAnalysisContext {
        protocol_state,
        protocol_warnings: &[],
        stdout_lines,
        stderr_lines,
        estimated_prompt_tokens: 0,
        estimated_output_tokens: 0,
    }
}

fn sample_analysis() -> IterationAnalysis {
    IterationAnalysis {
        completion_indicators: 2,
        has_explicit_exit_true: true,
        probable_progress: ProgressSignal::Moderate,
        ..IterationAnalysis::default()
    }
}

#[test]
fn analyze_session_outcome_delegates_to_iteration_analysis() {
    let protocol_state = ProtocolState {
        explicit_exit: Some(true),
        events: vec![ProtocolEvent::Exit { value: true }],
        ..ProtocolState::default()
    };
    let stdout_lines = vec!["Implementation complete".to_owned(), "All done".to_owned()];
    let stderr_lines = Vec::new();

    let analysis =
        analyze_session_outcome(base_context(&protocol_state, &stdout_lines, &stderr_lines));

    assert!(analysis.has_explicit_exit_true);
    assert_eq!(analysis.completion_indicators, 2);
}

#[test]
fn evaluate_exit_policy_uses_policy_logic() {
    let analysis = IterationAnalysis {
        completion_indicators: 2,
        has_explicit_exit_true: true,
        ..IterationAnalysis::default()
    };

    assert_eq!(
        evaluate_exit_policy(&ExitPolicy::default(), &analysis),
        ExitDecision::Success
    );
}

#[test]
fn evaluate_outcome_exit_policy_uses_outcome_analysis() {
    let started_at: Timestamp = "2026-03-17T08:00:00Z".parse().expect("timestamp");
    let outcome = SessionOutcome {
        session: ClaudeSessionRecord {
            id: SessionId::new("ses-1"),
            run_id: RunId::new("run-1"),
            provider: grove_types::RuntimeProvider::Claude,
            external_session_id: None,
            ordinal_in_run: 1,
            status: SessionStatus::Completed,
            started_at,
            ended_at: Some(started_at),
            prompt_id: None,
            prompt_manifest_path: None,
            prompt_bytes: 0,
            estimated_input_tokens: 0,
            estimated_output_tokens: 0,
            exit_code: Some(0),
            stop_reason: Some(StopReason::Exit),
            transcript_path: "transcripts/ses-1.jsonl".to_owned(),
        },
        protocol_events: vec![ProtocolEvent::Exit { value: true }],
        analysis: IterationAnalysis {
            completion_indicators: 2,
            has_explicit_exit_true: true,
            ..IterationAnalysis::default()
        },
        terminal_class: SessionTerminalClass::Success,
        context_pressure_pct: None,
        context_pressure_level: ContextPressureLevel::Ok,
        stdout_tail: Vec::new(),
        stderr_tail: Vec::new(),
    };

    assert_eq!(
        evaluate_outcome_exit_policy(&ExitPolicy::default(), &outcome),
        ExitDecision::Success
    );
}

#[test]
fn evaluate_outcome_exit_policy_respects_explicit_exit_false_override() {
    let started_at: Timestamp = "2026-03-17T08:00:00Z".parse().expect("timestamp");
    let outcome = SessionOutcome {
        session: ClaudeSessionRecord {
            id: SessionId::new("ses-2"),
            run_id: RunId::new("run-2"),
            provider: grove_types::RuntimeProvider::Claude,
            external_session_id: None,
            ordinal_in_run: 1,
            status: SessionStatus::UnknownFailure,
            started_at,
            ended_at: Some(started_at),
            prompt_id: None,
            prompt_manifest_path: None,
            prompt_bytes: 0,
            estimated_input_tokens: 0,
            estimated_output_tokens: 0,
            exit_code: Some(0),
            stop_reason: Some(StopReason::Unknown),
            transcript_path: "transcripts/ses-2.jsonl".to_owned(),
        },
        protocol_events: vec![
            ProtocolEvent::Exit { value: false },
            ProtocolEvent::Exit { value: true },
        ],
        analysis: IterationAnalysis {
            completion_indicators: 4,
            has_explicit_exit_true: true,
            has_explicit_exit_false: true,
            ..IterationAnalysis::default()
        },
        terminal_class: SessionTerminalClass::UnknownFailure,
        context_pressure_pct: None,
        context_pressure_level: ContextPressureLevel::Ok,
        stdout_tail: Vec::new(),
        stderr_tail: Vec::new(),
    };

    assert_eq!(
        evaluate_outcome_exit_policy(&ExitPolicy::default(), &outcome),
        ExitDecision::Continue
    );
}

#[test]
fn context_monitor_reports_warn_rotate_and_hard_stop() {
    let analysis = IterationAnalysis {
        estimated_prompt_tokens: 60,
        estimated_output_tokens: 30,
        ..IterationAnalysis::default()
    };
    let monitor = ContextMonitor::new(0.5, 0.7, 0.85, 400);

    let pressure = monitor.estimate(&analysis);
    assert_eq!(pressure.estimated_bytes, 360);
    assert_eq!(monitor.classify(&analysis), ContextPressureLevel::HardStop);
    assert_eq!(monitor.decide(&analysis), ContextPressureDecision::HardStop);
}

#[test]
fn classifies_checkpoint_before_other_outcomes() {
    let analysis = IterationAnalysis {
        checkpoint_emitted: true,
        permission_denials: 2,
        rate_limit_markers: 1,
        ..IterationAnalysis::default()
    };

    assert_eq!(
        classify_session_outcome(&analysis, Some(1), false),
        SessionTerminalClass::Checkpoint
    );
}

#[test]
fn checkpoint_beats_apparent_success_signals() {
    let analysis = IterationAnalysis {
        checkpoint_emitted: true,
        completion_indicators: 4,
        has_explicit_exit_true: true,
        ..IterationAnalysis::default()
    };

    assert_eq!(
        classify_session_outcome(&analysis, Some(0), false),
        SessionTerminalClass::Checkpoint
    );
}

#[test]
fn classifies_timeout_before_rate_limit() {
    let analysis = IterationAnalysis {
        rate_limit_markers: 2,
        ..IterationAnalysis::default()
    };

    assert_eq!(
        classify_session_outcome(&analysis, Some(1), true),
        SessionTerminalClass::Timeout
    );
}

#[test]
fn classifies_permission_denied_before_generic_failure() {
    let analysis = IterationAnalysis {
        permission_denials: 1,
        ..IterationAnalysis::default()
    };

    assert_eq!(
        classify_session_outcome(&analysis, Some(1), false),
        SessionTerminalClass::PermissionDenied
    );
}

#[test]
fn classifies_rate_limit_from_markers() {
    let analysis = IterationAnalysis {
        rate_limit_markers: 1,
        ..IterationAnalysis::default()
    };

    assert_eq!(
        classify_session_outcome(&analysis, Some(1), false),
        SessionTerminalClass::RateLimit
    );
}

#[test]
fn classifies_rate_limit_from_warning_text_without_markers() {
    let analysis = IterationAnalysis {
        warnings: vec!["rate-limit retry window still active".to_owned()],
        ..IterationAnalysis::default()
    };

    assert_eq!(
        classify_session_outcome(&analysis, Some(1), false),
        SessionTerminalClass::RateLimit
    );
}

#[test]
fn classifies_success_only_after_clean_exit_path() {
    assert_eq!(
        classify_session_outcome(&sample_analysis(), Some(0), false),
        SessionTerminalClass::Success
    );
    assert_eq!(
        classify_session_outcome(&sample_analysis(), Some(1), false),
        SessionTerminalClass::Crash
    );
}

#[test]
fn explicit_exit_false_prevents_success_classification() {
    let mut analysis = sample_analysis();
    analysis.has_explicit_exit_false = true;

    assert_eq!(
        classify_session_outcome(&analysis, Some(0), false),
        SessionTerminalClass::UnknownFailure
    );
}

#[test]
fn plan_approval_style_output_stays_unknown_failure() {
    let analysis = analyze_session_outcome(base_context(
        &ProtocolState::default(),
        &["Implemented the plan file and requested approval.".to_owned()],
        &[],
    ));

    assert_eq!(analysis.probable_progress, ProgressSignal::None);
    assert_eq!(
        classify_session_outcome(&analysis, Some(0), false),
        SessionTerminalClass::UnknownFailure
    );
}

#[test]
fn no_progress_threshold_opens_breaker() {
    let now: Timestamp = "2026-03-17T08:00:00Z".parse().expect("timestamp");
    let analysis = IterationAnalysis::default();
    let state = CircuitBreakerState {
        no_progress_count: 2,
        ..CircuitBreakerState::default()
    };

    let next = update_circuit_breaker(state, &analysis, now, 3, 5, 2, 30);
    assert_eq!(next.state, CircuitState::Open);
    assert_eq!(next.opened_at, Some(now));
}

#[test]
fn same_error_threshold_opens_breaker() {
    let now: Timestamp = "2026-03-17T08:00:00Z".parse().expect("timestamp");
    let analysis = IterationAnalysis {
        repeated_error_fingerprint: Some("error: failed to open transcript".to_owned()),
        ..IterationAnalysis::default()
    };
    let state = CircuitBreakerState {
        same_error_count: 4,
        last_error_fingerprint: Some("error: failed to open transcript".to_owned()),
        ..CircuitBreakerState::default()
    };

    let next = update_circuit_breaker(state, &analysis, now, 3, 5, 2, 30);
    assert_eq!(next.state, CircuitState::Open);
}

#[test]
fn permission_denial_threshold_opens_breaker() {
    let now: Timestamp = "2026-03-17T08:00:00Z".parse().expect("timestamp");
    let analysis = IterationAnalysis {
        permission_denials: 1,
        ..IterationAnalysis::default()
    };
    let state = CircuitBreakerState {
        permission_denial_count: 1,
        ..CircuitBreakerState::default()
    };

    let next = update_circuit_breaker(state, &analysis, now, 3, 5, 2, 30);
    assert_eq!(next.state, CircuitState::Open);
}

#[test]
fn progress_resets_closed_counters() {
    let now: Timestamp = "2026-03-17T08:00:00Z".parse().expect("timestamp");
    let analysis = IterationAnalysis {
        probable_progress: ProgressSignal::Strong,
        ..IterationAnalysis::default()
    };
    let state = CircuitBreakerState {
        no_progress_count: 2,
        same_error_count: 4,
        permission_denial_count: 1,
        last_error_fingerprint: Some("error: failed to open transcript".to_owned()),
        opened_at: Some(now),
        ..CircuitBreakerState::default()
    };

    let next = update_circuit_breaker(state, &analysis, now, 3, 5, 2, 30);
    assert_eq!(next, CircuitBreakerState::default());
}

#[test]
fn different_error_fingerprint_resets_same_error_count() {
    let now: Timestamp = "2026-03-17T08:00:00Z".parse().expect("timestamp");
    let analysis = IterationAnalysis {
        repeated_error_fingerprint: Some("error: second".to_owned()),
        ..IterationAnalysis::default()
    };
    let state = CircuitBreakerState {
        same_error_count: 4,
        last_error_fingerprint: Some("error: first".to_owned()),
        ..CircuitBreakerState::default()
    };

    let next = update_circuit_breaker(state, &analysis, now, 10, 5, 2, 30);
    assert_eq!(next.state, CircuitState::Closed);
    assert_eq!(next.same_error_count, 1);
}

#[test]
fn open_to_half_open_after_cooldown() {
    let opened_at: Timestamp = "2026-03-17T08:00:00Z".parse().expect("timestamp");
    let now: Timestamp = "2026-03-17T08:31:00Z".parse().expect("timestamp");
    let state = CircuitBreakerState {
        state: CircuitState::Open,
        opened_at: Some(opened_at),
        ..CircuitBreakerState::default()
    };

    let next = update_circuit_breaker(state, &IterationAnalysis::default(), now, 3, 5, 2, 30);
    assert_eq!(next.state, CircuitState::HalfOpen);
}

#[test]
fn cooldown_not_expired_stays_open() {
    let opened_at: Timestamp = "2026-03-17T08:00:00Z".parse().expect("timestamp");
    let now: Timestamp = "2026-03-17T08:10:00Z".parse().expect("timestamp");
    let state = CircuitBreakerState {
        state: CircuitState::Open,
        opened_at: Some(opened_at),
        ..CircuitBreakerState::default()
    };

    let next = update_circuit_breaker(
        state.clone(),
        &IterationAnalysis::default(),
        now,
        3,
        5,
        2,
        30,
    );
    assert_eq!(next, state);
}

#[test]
fn half_open_to_closed_on_progress() {
    let now: Timestamp = "2026-03-17T08:31:00Z".parse().expect("timestamp");
    let state = CircuitBreakerState {
        state: CircuitState::HalfOpen,
        ..CircuitBreakerState::default()
    };
    let analysis = IterationAnalysis {
        probable_progress: ProgressSignal::Strong,
        ..IterationAnalysis::default()
    };

    let next = update_circuit_breaker(state, &analysis, now, 3, 5, 2, 30);
    assert_eq!(next, CircuitBreakerState::default());
}

#[test]
fn half_open_to_open_on_failure() {
    let now: Timestamp = "2026-03-17T08:31:00Z".parse().expect("timestamp");
    let state = CircuitBreakerState {
        state: CircuitState::HalfOpen,
        ..CircuitBreakerState::default()
    };

    let next = update_circuit_breaker(state, &IterationAnalysis::default(), now, 3, 5, 2, 30);
    assert_eq!(next.state, CircuitState::Open);
    assert_eq!(next.opened_at, Some(now));
}
