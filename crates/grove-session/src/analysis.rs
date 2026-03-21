#![allow(clippy::unwrap_used, clippy::expect_used)]
use crate::{ProtocolWarning, infer_progress_signal};
use grove_types::{IterationAnalysis, ProtocolEvent, ProtocolState};
use std::collections::HashMap;

const COMPLETION_PHRASES: [&str; 7] = [
    "all tasks complete",
    "implementation complete",
    "project ready",
    "all done",
    "completed successfully",
    "finished the task",
    "done with this task",
];

const PERMISSION_DENIAL_PATTERNS: [&str; 8] = [
    "permission denied",
    "operation not permitted",
    "tool use rejected",
    "tool rejected",
    "rejected by user",
    "user denied",
    "approval denied",
    "denied by user",
];

const RATE_LIMIT_PATTERNS: [&str; 4] = ["rate limit", "rate-limit", "ratelimit", "rate limited"];

const ERROR_PATTERNS: [&str; 10] = [
    "error",
    "failed",
    "exception",
    "panic",
    "permission denied",
    "operation not permitted",
    "rate limit",
    "rate limited",
    "tool rejected",
    "rejected by user",
];

#[derive(Debug, Clone, Copy)]
pub struct AnalysisInput<'a> {
    pub protocol_state: &'a ProtocolState,
    pub protocol_warnings: &'a [ProtocolWarning],
    pub stdout_lines: &'a [String],
    pub stderr_lines: &'a [String],
    pub estimated_prompt_tokens: u32,
    pub estimated_output_tokens: u32,
}

#[must_use]
pub fn analyze_iteration(input: AnalysisInput<'_>) -> IterationAnalysis {
    let repeated_error_fingerprint =
        detect_repeated_error_fingerprint(input.stdout_lines, input.stderr_lines);
    let warnings = merged_warnings(input.protocol_state, input.protocol_warnings);

    IterationAnalysis {
        output_lines: input.stdout_lines.len() + input.stderr_lines.len(),
        output_chars: total_output_chars(input.stdout_lines, input.stderr_lines),
        completion_indicators: count_completion_indicators(input.stdout_lines),
        has_explicit_exit_true: has_explicit_exit(input.protocol_state, true),
        has_explicit_exit_false: has_explicit_exit(input.protocol_state, false),
        checkpoint_emitted: checkpoint_emitted(input.protocol_state),
        probable_progress: infer_progress_signal(
            input.protocol_state,
            input.stdout_lines,
            input.stderr_lines,
            repeated_error_fingerprint.as_deref(),
        ),
        permission_denials: count_permission_denials(input.stdout_lines, input.stderr_lines),
        rate_limit_markers: count_rate_limit_markers(
            input.stdout_lines,
            input.stderr_lines,
            &warnings,
        ),
        repeated_error_fingerprint,
        artifacts_mentioned: input.protocol_state.artifacts.clone(),
        lessons: input.protocol_state.lessons.clone(),
        decisions: input.protocol_state.decisions.clone(),
        warnings,
        estimated_prompt_tokens: input.estimated_prompt_tokens,
        estimated_output_tokens: input.estimated_output_tokens,
    }
}

fn total_output_chars(stdout_lines: &[String], stderr_lines: &[String]) -> usize {
    stdout_lines.iter().map(|line| line.len()).sum::<usize>()
        + stderr_lines.iter().map(|line| line.len()).sum::<usize>()
}

fn count_completion_indicators(stdout_lines: &[String]) -> u32 {
    stdout_lines
        .iter()
        .filter(|line| contains_completion_phrase(line))
        .count() as u32
}

fn contains_completion_phrase(line: &str) -> bool {
    let normalized = normalize_line(line);
    COMPLETION_PHRASES
        .iter()
        .any(|phrase| normalized.contains(phrase))
}

fn has_explicit_exit(protocol_state: &ProtocolState, expected: bool) -> bool {
    protocol_state
        .events
        .iter()
        .any(|event| matches!(event, ProtocolEvent::Exit { value } if *value == expected))
        || protocol_state.explicit_exit == Some(expected)
}

fn checkpoint_emitted(protocol_state: &ProtocolState) -> bool {
    protocol_state.latest_checkpoint.is_some()
        || protocol_state
            .events
            .iter()
            .any(|event| matches!(event, ProtocolEvent::Checkpoint { .. }))
}

fn count_permission_denials(stdout_lines: &[String], stderr_lines: &[String]) -> u32 {
    stdout_lines
        .iter()
        .chain(stderr_lines.iter())
        .filter(|line| is_permission_denial(line))
        .count() as u32
}

fn is_permission_denial(line: &str) -> bool {
    let normalized = normalize_line(line);
    PERMISSION_DENIAL_PATTERNS
        .iter()
        .any(|pattern| normalized.contains(pattern))
}

fn count_rate_limit_markers(
    stdout_lines: &[String],
    stderr_lines: &[String],
    warnings: &[String],
) -> u32 {
    stdout_lines
        .iter()
        .chain(stderr_lines.iter())
        .filter(|line| is_rate_limit_marker(line))
        .count() as u32
        + warnings
            .iter()
            .filter(|warning| is_rate_limit_marker(warning))
            .count() as u32
}

fn is_rate_limit_marker(line: &str) -> bool {
    let normalized = normalize_line(line);
    RATE_LIMIT_PATTERNS
        .iter()
        .any(|pattern| normalized.contains(pattern))
}

fn detect_repeated_error_fingerprint(
    stdout_lines: &[String],
    stderr_lines: &[String],
) -> Option<String> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut order = Vec::new();

    for fingerprint in stdout_lines
        .iter()
        .chain(stderr_lines.iter())
        .filter_map(|line| error_fingerprint_candidate(line))
    {
        let count = counts.entry(fingerprint.clone()).or_insert_with(|| {
            order.push(fingerprint.clone());
            0
        });
        *count += 1;
    }

    let mut selected = None;
    let mut max_count = 1;
    for fingerprint in order {
        let count = counts.get(&fingerprint).copied().unwrap_or(0);
        if count > max_count {
            max_count = count;
            selected = Some(fingerprint);
        }
    }
    selected
}

fn error_fingerprint_candidate(line: &str) -> Option<String> {
    let normalized = normalize_line(line);
    if normalized.is_empty() {
        return None;
    }

    if ERROR_PATTERNS
        .iter()
        .any(|pattern| normalized.contains(pattern))
    {
        Some(normalized)
    } else {
        None
    }
}

fn merged_warnings(
    protocol_state: &ProtocolState,
    protocol_warnings: &[ProtocolWarning],
) -> Vec<String> {
    let mut warnings = protocol_state.warnings.clone();
    for warning in protocol_warnings {
        push_unique(&mut warnings, warning.reason.clone());
    }
    warnings
}

fn push_unique(items: &mut Vec<String>, candidate: String) {
    if !items.iter().any(|item| item == &candidate) {
        items.push(candidate);
    }
}

fn normalize_line(line: &str) -> String {
    line.split_whitespace()
        .map(|part| part.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]

mod tests {
    use super::*;
    use grove_types::{CheckpointPayload, ProtocolEvent};
    use serde_json::json;

    fn checkpoint_payload() -> CheckpointPayload {
        CheckpointPayload {
            progress: "halfway".to_owned(),
            next_step: "finish wiring".to_owned(),
            context: json!({}),
            open_questions: Vec::new(),
            claimed_paths: Vec::new(),
            confidence: None,
        }
    }

    fn protocol_state_with_events(events: Vec<ProtocolEvent>) -> ProtocolState {
        ProtocolState {
            result_summary: Some("implemented runtime analysis".to_owned()),
            artifacts: vec!["crates/grove-session/src/analysis.rs".to_owned()],
            lessons: vec!["keep exit conservative".to_owned()],
            decisions: vec!["reuse parser state".to_owned()],
            warnings: vec!["mirror still pending".to_owned()],
            explicit_exit: Some(true),
            latest_checkpoint: Some(checkpoint_payload()),
            events,
        }
    }

    #[test]
    fn analysis_populates_protocol_derived_fields() {
        let protocol_state = protocol_state_with_events(vec![
            ProtocolEvent::Result {
                summary: "implemented runtime analysis".to_owned(),
            },
            ProtocolEvent::Exit { value: true },
            ProtocolEvent::Checkpoint {
                payload: checkpoint_payload(),
            },
        ]);
        let stdout_lines = vec![
            "Implementation complete after validating the transcript path".to_owned(),
            "All done".to_owned(),
        ];
        let stderr_lines = vec!["permission denied while opening sandboxed path".to_owned()];
        let protocol_warnings = vec![ProtocolWarning {
            line: 9,
            raw_line: "GROVE_EXIT: maybe".to_owned(),
            reason: "invalid GROVE_EXIT value `maybe`; expected true or false".to_owned(),
        }];

        let analysis = analyze_iteration(AnalysisInput {
            protocol_state: &protocol_state,
            protocol_warnings: &protocol_warnings,
            stdout_lines: &stdout_lines,
            stderr_lines: &stderr_lines,
            estimated_prompt_tokens: 144,
            estimated_output_tokens: 89,
        });

        assert_eq!(analysis.output_lines, 3);
        assert_eq!(analysis.completion_indicators, 2);
        assert!(analysis.has_explicit_exit_true);
        assert!(!analysis.has_explicit_exit_false);
        assert!(analysis.checkpoint_emitted);
        assert_eq!(analysis.artifacts_mentioned, protocol_state.artifacts);
        assert_eq!(analysis.lessons, protocol_state.lessons);
        assert_eq!(analysis.decisions, protocol_state.decisions);
        assert_eq!(analysis.permission_denials, 1);
        assert_eq!(analysis.estimated_prompt_tokens, 144);
        assert_eq!(analysis.estimated_output_tokens, 89);
        assert!(
            analysis
                .warnings
                .contains(&"mirror still pending".to_owned())
        );
        assert!(
            analysis
                .warnings
                .contains(&"invalid GROVE_EXIT value `maybe`; expected true or false".to_owned())
        );
    }

    #[test]
    fn each_completion_phrase_increments_indicator_count() {
        let stdout_lines = COMPLETION_PHRASES
            .iter()
            .map(|phrase| phrase.to_string())
            .collect::<Vec<_>>();

        let analysis = analyze_iteration(AnalysisInput {
            protocol_state: &ProtocolState::default(),
            protocol_warnings: &[],
            stdout_lines: &stdout_lines,
            stderr_lines: &[],
            estimated_prompt_tokens: 0,
            estimated_output_tokens: 0,
        });

        assert_eq!(
            analysis.completion_indicators,
            COMPLETION_PHRASES.len() as u32
        );
    }

    #[test]
    fn explicit_exit_false_is_recorded_from_protocol_history() {
        let protocol_state = ProtocolState {
            explicit_exit: Some(true),
            events: vec![
                ProtocolEvent::Exit { value: false },
                ProtocolEvent::Exit { value: true },
            ],
            ..ProtocolState::default()
        };

        let analysis = analyze_iteration(AnalysisInput {
            protocol_state: &protocol_state,
            protocol_warnings: &[],
            stdout_lines: &[],
            stderr_lines: &[],
            estimated_prompt_tokens: 0,
            estimated_output_tokens: 0,
        });

        assert!(analysis.has_explicit_exit_true);
        assert!(analysis.has_explicit_exit_false);
    }

    #[test]
    fn permission_denials_are_counted_across_stdout_and_stderr() {
        let stdout_lines = vec![
            "Tool use rejected by policy".to_owned(),
            "Operation not permitted while creating temp directory".to_owned(),
        ];
        let stderr_lines = vec!["Permission denied opening transcript".to_owned()];

        let analysis = analyze_iteration(AnalysisInput {
            protocol_state: &ProtocolState::default(),
            protocol_warnings: &[],
            stdout_lines: &stdout_lines,
            stderr_lines: &stderr_lines,
            estimated_prompt_tokens: 0,
            estimated_output_tokens: 0,
        });

        assert_eq!(analysis.permission_denials, 3);
    }

    #[test]
    fn repeated_identical_error_lines_produce_fingerprint() {
        let stderr_lines = vec![
            "Error: Failed to open transcript".to_owned(),
            "  error: failed   to open transcript  ".to_owned(),
        ];

        let analysis = analyze_iteration(AnalysisInput {
            protocol_state: &ProtocolState::default(),
            protocol_warnings: &[],
            stdout_lines: &[],
            stderr_lines: &stderr_lines,
            estimated_prompt_tokens: 0,
            estimated_output_tokens: 0,
        });

        assert_eq!(
            analysis.repeated_error_fingerprint.as_deref(),
            Some("error: failed to open transcript")
        );
    }

    #[test]
    fn checkpoint_presence_sets_checkpoint_emitted() {
        let protocol_state = ProtocolState {
            latest_checkpoint: Some(checkpoint_payload()),
            ..ProtocolState::default()
        };

        let analysis = analyze_iteration(AnalysisInput {
            protocol_state: &protocol_state,
            protocol_warnings: &[],
            stdout_lines: &[],
            stderr_lines: &[],
            estimated_prompt_tokens: 0,
            estimated_output_tokens: 0,
        });

        assert!(analysis.checkpoint_emitted);
    }

    #[test]
    fn rate_limit_markers_are_counted_from_output_and_warnings() {
        let stdout_lines = vec![
            "Rate limit exceeded while requesting model output".to_owned(),
            "temporary rate-limit backoff triggered".to_owned(),
        ];
        let stderr_lines = vec!["ratelimit retry window still active".to_owned()];
        let protocol_warnings = vec![ProtocolWarning {
            line: 12,
            raw_line: "warning".to_owned(),
            reason: "rate limit warning surfaced from parser".to_owned(),
        }];

        let analysis = analyze_iteration(AnalysisInput {
            protocol_state: &ProtocolState::default(),
            protocol_warnings: &protocol_warnings,
            stdout_lines: &stdout_lines,
            stderr_lines: &stderr_lines,
            estimated_prompt_tokens: 0,
            estimated_output_tokens: 0,
        });

        assert_eq!(analysis.rate_limit_markers, 4);
    }
}
