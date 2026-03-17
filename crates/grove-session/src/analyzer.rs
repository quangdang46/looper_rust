use crate::{analyze_iteration, AnalysisInput, ExitDecision, ExitPolicy, ProtocolWarning};
use grove_types::{IterationAnalysis, ProtocolState, SessionOutcome};

#[derive(Debug, Clone, Copy)]
pub struct SessionAnalysisContext<'a> {
    pub protocol_state: &'a ProtocolState,
    pub protocol_warnings: &'a [ProtocolWarning],
    pub stdout_lines: &'a [String],
    pub stderr_lines: &'a [String],
    pub estimated_prompt_tokens: u32,
    pub estimated_output_tokens: u32,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ExitPolicy;
    use grove_types::{
        ClaudeSessionRecord, ProtocolEvent, ProtocolState, RunId, SessionId, SessionOutcome,
        SessionStatus, SessionTerminalClass, StopReason, Timestamp,
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
            stdout_tail: Vec::new(),
            stderr_tail: Vec::new(),
        };

        assert_eq!(
            evaluate_outcome_exit_policy(&ExitPolicy::default(), &outcome),
            ExitDecision::Success
        );
    }
}
