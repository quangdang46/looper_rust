#![allow(clippy::unwrap_used, clippy::expect_used)]
use grove_types::{CheckpointPayload, ProgressSignal, ProtocolState};

const SUBSTANTIAL_STDOUT_LINE_THRESHOLD: usize = 3;
const SUBSTANTIAL_STDOUT_CHAR_THRESHOLD: usize = 120;

#[must_use]
pub fn infer_progress_signal(
    protocol_state: &ProtocolState,
    stdout_lines: &[String],
    _stderr_lines: &[String],
    repeated_error_fingerprint: Option<&str>,
) -> ProgressSignal {
    let structured_categories = structured_category_count(protocol_state);
    if structured_categories >= 2 {
        return ProgressSignal::Strong;
    }

    if has_moderate_protocol_evidence(protocol_state) {
        return ProgressSignal::Moderate;
    }

    if structured_categories == 1 {
        return ProgressSignal::Weak;
    }

    if repeated_error_fingerprint.is_some() {
        return ProgressSignal::None;
    }

    if has_substantial_stdout_output(stdout_lines) {
        ProgressSignal::Weak
    } else {
        ProgressSignal::None
    }
}

fn structured_category_count(protocol_state: &ProtocolState) -> usize {
    let mut categories = 0;
    if protocol_state
        .result_summary
        .as_ref()
        .is_some_and(|summary| !summary.trim().is_empty())
    {
        categories += 1;
    }
    if !protocol_state.artifacts.is_empty() {
        categories += 1;
    }
    if !protocol_state.lessons.is_empty() {
        categories += 1;
    }
    if !protocol_state.decisions.is_empty() {
        categories += 1;
    }
    if !protocol_state.warnings.is_empty() {
        categories += 1;
    }
    if has_valid_checkpoint(protocol_state.latest_checkpoint.as_ref()) {
        categories += 1;
    }
    categories
}

fn has_moderate_protocol_evidence(protocol_state: &ProtocolState) -> bool {
    protocol_state
        .result_summary
        .as_ref()
        .is_some_and(|summary| !summary.trim().is_empty())
        || !protocol_state.artifacts.is_empty()
        || has_valid_checkpoint(protocol_state.latest_checkpoint.as_ref())
}

fn has_valid_checkpoint(checkpoint: Option<&CheckpointPayload>) -> bool {
    checkpoint.is_some_and(|payload| {
        !payload.progress.trim().is_empty() && !payload.next_step.trim().is_empty()
    })
}

fn has_substantial_stdout_output(stdout_lines: &[String]) -> bool {
    let non_empty_lines = stdout_lines
        .iter()
        .filter(|line| !line.trim().is_empty())
        .count();
    let output_chars: usize = stdout_lines
        .iter()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.len())
        .sum();

    non_empty_lines >= SUBSTANTIAL_STDOUT_LINE_THRESHOLD
        || output_chars >= SUBSTANTIAL_STDOUT_CHAR_THRESHOLD
}

#[cfg(test)]
mod tests;
