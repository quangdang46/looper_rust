#![allow(clippy::unwrap_used, clippy::expect_used)]
use crate::{AnalysisInput, ExitDecision, ExitPolicy, ProtocolWarning, analyze_iteration};
use chrono::Duration;
use grove_types::{
    CircuitBreakerState, CircuitState, ContextPressureLevel, IterationAnalysis, ProgressSignal,
    ProtocolState, SessionOutcome, SessionTerminalClass, Timestamp,
};

#[derive(Debug, Clone, Copy)]
pub struct SessionAnalysisContext<'a> {
    pub protocol_state: &'a ProtocolState,
    pub protocol_warnings: &'a [ProtocolWarning],
    pub stdout_lines: &'a [String],
    pub stderr_lines: &'a [String],
    pub estimated_prompt_tokens: u32,
    pub estimated_output_tokens: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContextMonitor {
    pub warn_pct: f32,
    pub rotate_pct: f32,
    pub hard_stop_pct: f32,
    pub max_context_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContextPressure {
    pub usage_pct: f32,
    pub estimated_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextPressureDecision {
    Ok,
    Warn,
    Rotate,
    HardStop,
}

#[must_use]
pub fn analyze_session_outcome(context: SessionAnalysisContext<'_>) -> IterationAnalysis {
    analyze_iteration(AnalysisInput {
        protocol_state: context.protocol_state,
        protocol_warnings: context.protocol_warnings,
        stdout_lines: context.stdout_lines,
        stderr_lines: context.stderr_lines,
        estimated_prompt_tokens: context.estimated_prompt_tokens,
        estimated_output_tokens: context.estimated_output_tokens,
    })
}

#[must_use]
pub fn evaluate_exit_policy(policy: &ExitPolicy, analysis: &IterationAnalysis) -> ExitDecision {
    policy.evaluate(analysis)
}

#[must_use]
pub fn evaluate_outcome_exit_policy(policy: &ExitPolicy, outcome: &SessionOutcome) -> ExitDecision {
    policy.evaluate(&outcome.analysis)
}

impl ContextMonitor {
    #[must_use]
    pub fn new(
        warn_pct: f32,
        rotate_pct: f32,
        hard_stop_pct: f32,
        max_context_bytes: usize,
    ) -> Self {
        Self {
            warn_pct,
            rotate_pct,
            hard_stop_pct,
            max_context_bytes,
        }
    }

    #[must_use]
    pub fn estimate(&self, analysis: &IterationAnalysis) -> ContextPressure {
        let estimated_bytes = ((analysis.estimated_prompt_tokens as usize)
            + (analysis.estimated_output_tokens as usize))
            .saturating_mul(4);
        let usage_pct = if self.max_context_bytes == 0 {
            0.0
        } else {
            estimated_bytes as f32 / self.max_context_bytes as f32
        };

        ContextPressure {
            usage_pct,
            estimated_bytes,
        }
    }

    #[must_use]
    pub fn classify(&self, analysis: &IterationAnalysis) -> ContextPressureLevel {
        let pressure = self.estimate(analysis);
        if pressure.usage_pct >= self.hard_stop_pct {
            ContextPressureLevel::HardStop
        } else if pressure.usage_pct >= self.rotate_pct {
            ContextPressureLevel::Rotate
        } else if pressure.usage_pct >= self.warn_pct {
            ContextPressureLevel::Warn
        } else {
            ContextPressureLevel::Ok
        }
    }

    #[must_use]
    pub fn decide(&self, analysis: &IterationAnalysis) -> ContextPressureDecision {
        match self.classify(analysis) {
            ContextPressureLevel::Ok => ContextPressureDecision::Ok,
            ContextPressureLevel::Warn => ContextPressureDecision::Warn,
            ContextPressureLevel::Rotate => ContextPressureDecision::Rotate,
            ContextPressureLevel::HardStop => ContextPressureDecision::HardStop,
        }
    }
}

#[must_use]
pub fn classify_session_outcome(
    analysis: &IterationAnalysis,
    exit_code: Option<i32>,
    timed_out: bool,
) -> SessionTerminalClass {
    classify_session_outcome_with_policy(&ExitPolicy::default(), analysis, exit_code, timed_out)
}

#[must_use]
pub fn classify_session_outcome_with_policy(
    policy: &ExitPolicy,
    analysis: &IterationAnalysis,
    exit_code: Option<i32>,
    timed_out: bool,
) -> SessionTerminalClass {
    if analysis.checkpoint_emitted {
        return SessionTerminalClass::Checkpoint;
    }
    if timed_out {
        return SessionTerminalClass::Timeout;
    }
    if analysis.permission_denials > 0 {
        return SessionTerminalClass::PermissionDenied;
    }
    if analysis.rate_limit_markers > 0 || has_rate_limit_warning(analysis) {
        return SessionTerminalClass::RateLimit;
    }
    if matches!(exit_code, Some(code) if code != 0) {
        return SessionTerminalClass::Crash;
    }
    if policy.evaluate(analysis) == ExitDecision::Success {
        return SessionTerminalClass::Success;
    }
    SessionTerminalClass::UnknownFailure
}

#[must_use]
pub fn update_circuit_breaker(
    state: CircuitBreakerState,
    analysis: &IterationAnalysis,
    now: Timestamp,
    no_progress_threshold: u32,
    same_error_threshold: u32,
    permission_denial_threshold: u32,
    cooldown_minutes: u64,
) -> CircuitBreakerState {
    match state.state {
        CircuitState::Open => update_open_circuit(state, now, cooldown_minutes),
        CircuitState::HalfOpen => update_half_open_circuit(analysis, now),
        CircuitState::Closed => update_closed_circuit(
            state,
            analysis,
            now,
            no_progress_threshold,
            same_error_threshold,
            permission_denial_threshold,
        ),
    }
}

fn update_open_circuit(
    state: CircuitBreakerState,
    now: Timestamp,
    cooldown_minutes: u64,
) -> CircuitBreakerState {
    let Some(opened_at) = state.opened_at else {
        return state;
    };

    if now.signed_duration_since(opened_at) >= Duration::minutes(cooldown_minutes as i64) {
        CircuitBreakerState {
            state: CircuitState::HalfOpen,
            ..state
        }
    } else {
        state
    }
}

fn update_half_open_circuit(analysis: &IterationAnalysis, now: Timestamp) -> CircuitBreakerState {
    if has_progress(analysis) {
        CircuitBreakerState::default()
    } else {
        CircuitBreakerState {
            state: CircuitState::Open,
            last_error_fingerprint: analysis.repeated_error_fingerprint.clone(),
            opened_at: Some(now),
            ..CircuitBreakerState::default()
        }
    }
}

fn update_closed_circuit(
    state: CircuitBreakerState,
    analysis: &IterationAnalysis,
    now: Timestamp,
    no_progress_threshold: u32,
    same_error_threshold: u32,
    permission_denial_threshold: u32,
) -> CircuitBreakerState {
    if has_progress(analysis) {
        return CircuitBreakerState::default();
    }

    let next_error_fingerprint = analysis.repeated_error_fingerprint.clone();
    let same_error_count = match next_error_fingerprint.as_deref() {
        Some(fingerprint) if state.last_error_fingerprint.as_deref() == Some(fingerprint) => {
            state.same_error_count.saturating_add(1)
        }
        Some(_) => 1,
        None => 0,
    };
    let permission_denial_count = if analysis.permission_denials > 0 {
        state.permission_denial_count.saturating_add(1)
    } else {
        0
    };
    let next = CircuitBreakerState {
        state: CircuitState::Closed,
        no_progress_count: state.no_progress_count.saturating_add(1),
        same_error_count,
        permission_denial_count,
        last_error_fingerprint: next_error_fingerprint,
        opened_at: None,
    };

    if next.no_progress_count >= no_progress_threshold
        || next.same_error_count >= same_error_threshold
        || next.permission_denial_count >= permission_denial_threshold
    {
        CircuitBreakerState {
            state: CircuitState::Open,
            opened_at: Some(now),
            ..next
        }
    } else {
        next
    }
}

fn has_progress(analysis: &IterationAnalysis) -> bool {
    !matches!(analysis.probable_progress, ProgressSignal::None)
}

fn has_rate_limit_warning(analysis: &IterationAnalysis) -> bool {
    analysis.warnings.iter().any(|warning| {
        let normalized = warning.to_ascii_lowercase();
        normalized.contains("rate limit")
            || normalized.contains("rate-limit")
            || normalized.contains("ratelimit")
    })
}

#[cfg(test)]
mod tests {
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
}
