
use super::*;
use grove_types::ProtocolState;
use serde_json::json;

fn checkpoint(progress: &str, next_step: &str) -> CheckpointPayload {
    CheckpointPayload {
        progress: progress.to_owned(),
        next_step: next_step.to_owned(),
        context: json!({}),
        open_questions: Vec::new(),
        claimed_paths: Vec::new(),
        confidence: None,
    }
}

#[test]
fn result_summary_alone_is_moderate() {
    let state = ProtocolState {
        result_summary: Some("implemented runtime analysis".to_owned()),
        ..ProtocolState::default()
    };

    assert_eq!(
        infer_progress_signal(&state, &[], &[], None),
        ProgressSignal::Moderate
    );
}

#[test]
fn artifacts_alone_are_moderate() {
    let state = ProtocolState {
        artifacts: vec!["src/main.rs".to_owned()],
        ..ProtocolState::default()
    };

    assert_eq!(
        infer_progress_signal(&state, &[], &[], None),
        ProgressSignal::Moderate
    );
}

#[test]
fn valid_checkpoint_alone_is_moderate() {
    let state = ProtocolState {
        latest_checkpoint: Some(checkpoint("halfway", "finish wiring")),
        ..ProtocolState::default()
    };

    assert_eq!(
        infer_progress_signal(&state, &[], &[], None),
        ProgressSignal::Moderate
    );
}

#[test]
fn multiple_structured_categories_are_strong() {
    let state = ProtocolState {
        result_summary: Some("implemented runtime analysis".to_owned()),
        decisions: vec!["kept exit policy conservative".to_owned()],
        ..ProtocolState::default()
    };

    assert_eq!(
        infer_progress_signal(&state, &[], &[], None),
        ProgressSignal::Strong
    );
}

#[test]
fn single_non_direct_structured_category_is_weak() {
    let state = ProtocolState {
        lessons: vec!["emit explicit exit markers".to_owned()],
        ..ProtocolState::default()
    };

    assert_eq!(
        infer_progress_signal(&state, &[], &[], None),
        ProgressSignal::Weak
    );
}

#[test]
fn substantial_stdout_without_markers_is_weak() {
    let stdout = vec![
        "Inspecting runtime output".to_owned(),
        "Comparing protocol state against transcript evidence".to_owned(),
        "Refining the completion gate to avoid optimistic exits".to_owned(),
    ];

    assert_eq!(
        infer_progress_signal(&ProtocolState::default(), &stdout, &[], None),
        ProgressSignal::Weak
    );
}

#[test]
fn repeated_error_without_structured_markers_is_none() {
    assert_eq!(
        infer_progress_signal(
            &ProtocolState::default(),
            &[],
            &[],
            Some("error: failed to open transcript"),
        ),
        ProgressSignal::None
    );
}

#[test]
fn structured_markers_outrank_repeated_error_fallback() {
    let state = ProtocolState {
        artifacts: vec!["crates/grove-session/src/analysis.rs".to_owned()],
        ..ProtocolState::default()
    };

    assert_eq!(
        infer_progress_signal(&state, &[], &[], Some("error: failed to open transcript")),
        ProgressSignal::Moderate
    );
}
