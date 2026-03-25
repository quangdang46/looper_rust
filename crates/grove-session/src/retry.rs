#![allow(clippy::unwrap_used, clippy::expect_used)]
use grove_types::{ExecutionContract, FailureClass, ProgressSignal, SessionOutcome};

const INVALID_IMAGE_PATTERN: &str = "does not represent a valid image";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryMutationPlan {
    pub next_contract: ExecutionContract,
    pub retry_delta_summary: String,
    pub rescue_card: String,
}

#[must_use]
pub fn plan_retry_mutation(
    failure_class: FailureClass,
    previous_outcome: Option<&SessionOutcome>,
) -> RetryMutationPlan {
    RetryMutationPlan {
        next_contract: next_contract_for_failure(failure_class, previous_outcome),
        retry_delta_summary: retry_delta_summary_for_failure(failure_class, previous_outcome),
        rescue_card: rescue_card_for_failure(failure_class, previous_outcome),
    }
}

fn next_contract_for_failure(
    failure_class: FailureClass,
    previous_outcome: Option<&SessionOutcome>,
) -> ExecutionContract {
    match failure_class {
        FailureClass::Interrupted | FailureClass::BrMirrorFailed => ExecutionContract::Resume,
        FailureClass::ClaudeCrashed if had_progress(previous_outcome) => ExecutionContract::Resume,
        _ => ExecutionContract::RetryRescue,
    }
}

fn retry_delta_summary_for_failure(
    failure_class: FailureClass,
    previous_outcome: Option<&SessionOutcome>,
) -> String {
    let progress_clause =
        if previous_outcome.is_some_and(|outcome| outcome.analysis.has_explicit_exit_false) {
            " and keeps the retry focused on unfinished work the last attempt explicitly left open"
        } else if had_progress(previous_outcome) {
            " and preserves any durable partial progress instead of replaying setup"
        } else {
            ""
        };
    let invalid_image_input = had_invalid_image_input(previous_outcome);

    match failure_class {
        FailureClass::Timeout => format!(
            "Changed retry framing: narrowed the retry scope, biased the next attempt toward earlier checkpoints, and forced the highest-value remaining step first{progress_clause}."
        ),
        FailureClass::RateLimit => format!(
            "Changed retry framing: reduces tool churn, waits for the limit window to clear, and resumes with a lighter interaction pattern{progress_clause}."
        ),
        FailureClass::PermissionDenied => format!(
            "Changed retry framing: avoids the blocked operation first, prefers an already-allowed path, and only asks for the minimum approval if it is still required{progress_clause}."
        ),
        FailureClass::CircuitOpen | FailureClass::NoProgress => format!(
            "Changed retry framing: breaks the previous stuck loop, requires a different verification path, and forbids repeating the same debugging sequence verbatim{progress_clause}."
        ),
        FailureClass::RepeatedError => format!(
            "Changed retry framing: steers away from the repeated error path, adds a concrete root-cause check before continuing, and documents exactly what changed for the retry{progress_clause}."
        ),
        FailureClass::ProtocolMalformed => format!(
            "Changed retry framing: keeps the implementation path tight while explicitly re-emphasizing exact GROVE marker formatting and structured checkpoint/result emission{progress_clause}."
        ),
        FailureClass::ClaudeCrashed if invalid_image_input => format!(
            "Changed retry framing: avoid attaching or opening image inputs, validate existing artifacts and README links textually first, and only touch media paths if a text-only check cannot finish the task{progress_clause}."
        ),
        FailureClass::ClaudeCrashed => format!(
            "Changed retry framing: reconstructs state from the durable transcript tail, trims rework, and resumes with a safer step-by-step execution path{progress_clause}."
        ),
        FailureClass::BrMirrorFailed => {
            "Changed retry framing: resumes from the completed implementation state and focuses on reconstructing structured result artifacts instead of redoing code changes.".to_owned()
        }
        FailureClass::Interrupted => {
            "Changed retry framing: resumes from durable progress and restarts at the first unfinished step instead of replaying the whole attempt.".to_owned()
        }
        FailureClass::Unknown => format!(
            "Changed retry framing: forces a materially different next attempt with explicit verification and a concise explanation of the new path{progress_clause}."
        ),
    }
}

fn rescue_card_for_failure(
    failure_class: FailureClass,
    previous_outcome: Option<&SessionOutcome>,
) -> String {
    let invalid_image_input = had_invalid_image_input(previous_outcome);

    let mut lines = match failure_class {
        FailureClass::Timeout => vec![
            "- Do not replay the full attempt; continue with the smallest remaining step that can prove progress.".to_owned(),
            "- Checkpoint earlier if the task starts to sprawl again.".to_owned(),
        ],
        FailureClass::RateLimit => vec![
            "- Avoid hammering the same tools or endpoints back-to-back.".to_owned(),
            "- Keep the retry lightweight until the rate-limit window clears.".to_owned(),
        ],
        FailureClass::PermissionDenied => vec![
            "- Do not repeat the blocked operation unchanged.".to_owned(),
            "- Choose an allowed path first and ask for the minimum approval only if there is no safe alternative.".to_owned(),
        ],
        FailureClass::CircuitOpen | FailureClass::NoProgress => vec![
            "- Break the prior loop immediately; choose a different inspection or verification path.".to_owned(),
            "- Prove the new path with one concrete check before expanding scope.".to_owned(),
        ],
        FailureClass::RepeatedError => vec![
            "- Do not repeat the same failing path.".to_owned(),
            "- Isolate the root cause before retrying the broader workflow.".to_owned(),
        ],
        FailureClass::ProtocolMalformed => vec![
            "- Keep the work path tight and emit valid GROVE markers exactly as specified.".to_owned(),
            "- Double-check structured marker formatting before ending the attempt.".to_owned(),
        ],
        FailureClass::ClaudeCrashed if invalid_image_input => vec![
            "- Do not attach, open, or inspect image inputs on this retry.".to_owned(),
            "- Validate demo/media tasks by file existence and README linkage before touching binary assets.".to_owned(),
            "- If text-only validation is not enough, checkpoint instead of repeating the same image path.".to_owned(),
        ],
        FailureClass::ClaudeCrashed => vec![
            "- Start from the durable transcript tail instead of redoing completed setup.".to_owned(),
            "- Continue in smaller verified steps so a crash does not erase progress again.".to_owned(),
        ],
        FailureClass::BrMirrorFailed => vec![
            "- Do not re-implement completed code just to recover the mirror.".to_owned(),
            "- Reconstruct the final structured result, artifacts, and summary from durable state.".to_owned(),
        ],
        FailureClass::Interrupted => vec![
            "- Resume from the first unfinished step only.".to_owned(),
            "- Reuse durable progress and avoid replaying already-completed work.".to_owned(),
        ],
        FailureClass::Unknown => vec![
            "- Pick a materially different retry path instead of replaying the last attempt.".to_owned(),
            "- Add one concrete verification step before claiming success.".to_owned(),
        ],
    };

    if let Some(error) = repeated_error_excerpt(previous_outcome) {
        lines.push(format!("- Previous repeated error to avoid: {error}"));
    }

    if previous_outcome.is_some_and(|outcome| outcome.analysis.has_explicit_exit_false) {
        lines.push(
            "- The prior attempt emitted `GROVE_EXIT: false`; continue unfinished work instead of closing early.".to_owned(),
        );
    }

    lines.push(
        "- Finish with accurate GROVE protocol markers that reflect the new outcome.".to_owned(),
    );

    lines.join("\n")
}

fn had_progress(previous_outcome: Option<&SessionOutcome>) -> bool {
    previous_outcome
        .is_some_and(|outcome| !matches!(outcome.analysis.probable_progress, ProgressSignal::None))
}

fn had_invalid_image_input(previous_outcome: Option<&SessionOutcome>) -> bool {
    previous_outcome.is_some_and(|outcome| {
        outcome
            .stdout_tail
            .iter()
            .chain(outcome.stderr_tail.iter())
            .any(|line| line.to_ascii_lowercase().contains(INVALID_IMAGE_PATTERN))
    })
}

fn repeated_error_excerpt(previous_outcome: Option<&SessionOutcome>) -> Option<String> {
    previous_outcome
        .and_then(|outcome| outcome.analysis.repeated_error_fingerprint.as_deref())
        .map(compact_excerpt)
}

fn compact_excerpt(text: &str) -> String {
    const MAX_CHARS: usize = 96;

    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let count = compact.chars().count();
    if count <= MAX_CHARS {
        return compact;
    }

    compact.chars().take(MAX_CHARS - 1).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use super::plan_retry_mutation;
    use grove_types::{
        ClaudeSessionRecord, ContextPressureLevel, ExecutionContract, FailureClass,
        IterationAnalysis, ProgressSignal, ProtocolEvent, RunId, SessionId, SessionOutcome,
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
            "API Error: 400 The image data you provided does not represent a valid image"
                .to_owned(),
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
}
