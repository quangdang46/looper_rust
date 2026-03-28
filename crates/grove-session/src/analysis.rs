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
const INVALID_IMAGE_PATTERNS: [&str; 2] = [
    "image data you provided does not represent a valid image",
    "does not represent a valid image",
];

const ERROR_PATTERNS: [&str; 12] = [
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
    "requested approval",
    "plan file",
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

pub fn contains_invalid_image_input(stdout_lines: &[String], stderr_lines: &[String]) -> bool {
    stdout_lines
        .iter()
        .chain(stderr_lines.iter())
        .any(|line| is_invalid_image_input(line))
}

fn is_invalid_image_input(line: &str) -> bool {
    let normalized = normalize_line(line);
    INVALID_IMAGE_PATTERNS
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
mod tests;
