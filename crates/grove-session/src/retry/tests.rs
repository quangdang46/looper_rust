use super::plan_retry_mutation;
use grove_types::{
    ClaudeSessionRecord, ContextPressureLevel, ExecutionContract, FailureClass, IterationAnalysis,
    ProgressSignal, ProtocolEvent, RunId, RuntimeProvider, SessionId, SessionOutcome,
    SessionStatus, SessionTerminalClass, StopReason, Timestamp,
};

fn sample_outcome(
    analysis: IterationAnalysis,
    terminal_class: SessionTerminalClass,
) -> SessionOutcome {
    let started_at: Timestamp = chrono::Utc::now();
    SessionOutcome {
        session: ClaudeSessionRecord {
            id: SessionId::new("ses-1"),
            run_id: RunId::new("run-1"),
            provider: RuntimeProvider::Claude,
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
            exit_code: Some(1),
            stop_reason: Some(StopReason::Unknown),
            transcript_path: ".grove/transcripts/run-1/ses-1.jsonl".to_owned(),
        },
        protocol_events: vec![ProtocolEvent::Exit { value: false }],
        analysis,
        terminal_class,
        context_pressure_pct: None,
        context_pressure_level: ContextPressureLevel::Ok,
        stdout_tail: Vec::new(),
        stderr_tail: Vec::new(),
    }
}

#[test]
fn repeated_error_retry_mentions_fingerprint_and_uses_retry_rescue_contract() {
    let outcome = sample_outcome(
        IterationAnalysis {
            repeated_error_fingerprint: Some(
                "error: protocol marker was malformed and failed to parse".to_owned(),
            ),
            ..IterationAnalysis::default()
        },
        SessionTerminalClass::UnknownFailure,
    );

    let plan = plan_retry_mutation(FailureClass::RepeatedError, Some(&outcome));

    assert_eq!(plan.next_contract, ExecutionContract::RetryRescue);
    assert!(plan.retry_delta_summary.contains("repeated error path"));
    assert!(
        plan.rescue_card
            .contains("Previous repeated error to avoid")
    );
    assert!(plan.rescue_card.contains("failed to parse"));
}

#[test]
fn permission_denied_retry_avoids_repeating_blocked_operation() {
    let outcome = sample_outcome(
        IterationAnalysis {
            permission_denials: 1,
            has_explicit_exit_false: true,
            ..IterationAnalysis::default()
        },
        SessionTerminalClass::PermissionDenied,
    );

    let plan = plan_retry_mutation(FailureClass::PermissionDenied, Some(&outcome));

    assert_eq!(plan.next_contract, ExecutionContract::RetryRescue);
    assert!(
        plan.retry_delta_summary
            .contains("avoids the blocked operation")
    );
    assert!(
        plan.rescue_card
            .contains("Do not repeat the blocked operation unchanged")
    );
    assert!(plan.rescue_card.contains("`GROVE_EXIT: false`"));
}

#[test]
fn timeout_retry_biases_toward_smaller_scope_and_earlier_checkpoints() {
    let outcome = sample_outcome(
        IterationAnalysis {
            probable_progress: ProgressSignal::Moderate,
            ..IterationAnalysis::default()
        },
        SessionTerminalClass::Timeout,
    );

    let plan = plan_retry_mutation(FailureClass::Timeout, Some(&outcome));

    assert_eq!(plan.next_contract, ExecutionContract::RetryRescue);
    assert!(
        plan.retry_delta_summary
            .contains("narrowed the retry scope")
    );
    assert!(
        plan.retry_delta_summary
            .contains("preserves any durable partial progress")
    );
    assert!(plan.rescue_card.contains("Checkpoint earlier"));
}

#[test]
fn interrupted_retry_switches_to_resume_contract() {
    let outcome = sample_outcome(
        IterationAnalysis {
            probable_progress: ProgressSignal::Strong,
            ..IterationAnalysis::default()
        },
        SessionTerminalClass::Crash,
    );

    let plan = plan_retry_mutation(FailureClass::Interrupted, Some(&outcome));

    assert_eq!(plan.next_contract, ExecutionContract::Resume);
    assert!(
        plan.retry_delta_summary
            .contains("resumes from durable progress")
    );
    assert!(
        plan.rescue_card
            .contains("Resume from the first unfinished step only")
    );
}

#[test]
fn mirror_failure_resume_plan_focuses_on_reconstructing_result_not_redoing_code() {
    let plan = plan_retry_mutation(FailureClass::BrMirrorFailed, None);

    assert_eq!(plan.next_contract, ExecutionContract::Resume);
    assert!(
        plan.retry_delta_summary
            .contains("completed implementation state")
    );
    assert!(
        plan.rescue_card
            .contains("Do not re-implement completed code")
    );
}

#[test]
fn claude_crash_with_invalid_image_input_forces_text_only_retry_guidance() {
    let mut outcome = sample_outcome(IterationAnalysis::default(), SessionTerminalClass::Crash);
    outcome.stdout_tail = vec![
        "API Error: 400 The image data you provided does not represent a valid image".to_owned(),
    ];

    let plan = plan_retry_mutation(FailureClass::ClaudeCrashed, Some(&outcome));

    assert_eq!(plan.next_contract, ExecutionContract::RetryRescue);
    assert!(
        plan.retry_delta_summary
            .contains("avoid attaching or opening image inputs")
    );
    assert!(
        plan.rescue_card
            .contains("Do not attach, open, or inspect image inputs")
    );
}

#[test]
fn unknown_retry_still_produces_a_non_empty_plan_without_previous_outcome() {
    let plan = plan_retry_mutation(FailureClass::Unknown, None);

    assert_eq!(plan.next_contract, ExecutionContract::RetryRescue);
    assert!(!plan.retry_delta_summary.is_empty());
    assert!(!plan.rescue_card.is_empty());
    assert!(
        plan.rescue_card
            .contains("Finish with accurate GROVE protocol markers")
    );
}
