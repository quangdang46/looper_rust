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
mod tests;
