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

#[test]
fn detects_invalid_image_input_from_api_error_text() {
    let stdout = vec![
        "API Error: 400 The image data you provided does not represent a valid image".to_owned(),
    ];
    let stderr = Vec::new();

    assert!(contains_invalid_image_input(&stdout, &stderr));
}
