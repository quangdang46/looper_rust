
use super::{CheckpointPromptInput, PromptMaterializationInput, materialize_prompt};
use grove_types::{
    BeadId, CheckpointId, ExecutionContract, PromptId, PromptSegmentKind, PromptTrimReason, RunId,
    Timestamp,
};
use std::error::Error;

type TestResult = Result<(), Box<dyn Error>>;

fn sample_input(contract: ExecutionContract) -> PromptMaterializationInput {
    PromptMaterializationInput {
        prompt_id: PromptId::new("prompt-1"),
        bead_id: BeadId::new("grove-1"),
        run_id: RunId::new("run-1"),
        created_at: "2026-03-18T00:00:00Z"
            .parse::<Timestamp>()
            .expect("timestamp"),
        contract,
        task_title: "Implement prompt materialization".to_owned(),
        task_description: "Build the prompt from ordered context segments.".to_owned(),
        startup_prompt: Some("Custom startup instructions".to_owned()),
        reservation_hints: vec!["crates/grove-session/src/materializer.rs".to_owned()],
        parent_handoffs: vec!["Parent run established the session contract.".to_owned()],
        checkpoint: Some(CheckpointPromptInput {
            checkpoint_id: CheckpointId::new("chk-1"),
            progress: "halfway there".to_owned(),
            next_step: "finish the materializer".to_owned(),
            open_questions: vec!["Need retry delta summary?".to_owned()],
        }),
        protocol_block: "[GROVE PROTOCOL]\nGROVE_EXIT: true".to_owned(),
        rescue_card: None,
        token_budget: None,
        retry_delta_summary: None,
        retrieval_query: None,
        archive_bundle: None,
        playbook_rules: vec![],
        escalation_context: None,
    }
}

#[test]
fn materializer_orders_sections_stably() {
    let materialized = materialize_prompt(sample_input(ExecutionContract::Implement));
    let kinds = materialized
        .manifest
        .sections
        .iter()
        .filter(|section| section.included)
        .map(|section| section.kind)
        .collect::<Vec<_>>();

    assert_eq!(
        kinds,
        vec![
            PromptSegmentKind::StartupPrompt,
            PromptSegmentKind::Contract,
            PromptSegmentKind::Task,
            PromptSegmentKind::Reservation,
            PromptSegmentKind::ParentHandoff,
            PromptSegmentKind::Checkpoint,
            PromptSegmentKind::Protocol,
        ]
    );
}

#[test]
fn materializer_changes_contract_framing() {
    let implement = materialize_prompt(sample_input(ExecutionContract::Implement));
    let single = materialize_prompt(sample_input(ExecutionContract::SingleTask));
    let retry = materialize_prompt(sample_input(ExecutionContract::RetryRescue));

    assert!(
        implement
            .rendered_prompt
            .contains("Implement the requested change directly")
    );
    assert!(
        single
            .rendered_prompt
            .contains("without entering plan mode or asking for approval")
    );
    assert!(
        single
            .rendered_prompt
            .contains("emit GROVE_CHECKPOINT plus GROVE_EXIT: false")
    );
    assert!(
        retry
            .rendered_prompt
            .contains("Retry the task with a changed approach")
    );
    assert!(
        retry
            .rendered_prompt
            .contains("do not enter plan mode, do not ask for approval")
    );
    assert!(
        retry
            .rendered_prompt
            .contains("emit GROVE_CHECKPOINT plus GROVE_EXIT: false")
    );
}

#[test]
fn materializer_trims_optional_sections_in_specified_order() {
    let mut input = sample_input(ExecutionContract::Implement);
    input.rescue_card = Some("Avoid repeating the same parse path.".to_owned());
    input.token_budget = Some(35);

    let materialized = materialize_prompt(input);
    let trimmed = materialized
        .manifest
        .sections
        .iter()
        .filter_map(|section| section.trim_reason.map(|reason| (section.kind, reason)))
        .collect::<Vec<_>>();

    assert!(trimmed.contains(&(
        PromptSegmentKind::ParentHandoff,
        PromptTrimReason::VerboseParentHandoff,
    )));
    assert!(trimmed.contains(&(
        PromptSegmentKind::Reservation,
        PromptTrimReason::NonEssentialReservationHint,
    )));
    assert!(materialized.rendered_prompt.contains("[CHECKPOINT]"));
    assert!(materialized.rendered_prompt.contains("[GROVE PROTOCOL]"));
}

#[test]
fn materializer_includes_startup_prompt_first_and_never_trims_it() {
    let mut input = sample_input(ExecutionContract::Implement);
    input.startup_prompt = Some("First read ALL of AGENTS.md and README.md carefully.".to_owned());
    input.token_budget = Some(10);

    let materialized = materialize_prompt(input);
    let startup = materialized
        .manifest
        .sections
        .iter()
        .find(|section| section.kind == PromptSegmentKind::StartupPrompt)
        .expect("missing startup prompt section");

    assert!(startup.included);
    assert_eq!(startup.trim_reason, None);
    assert!(
        materialized
            .rendered_prompt
            .starts_with("First read ALL of AGENTS.md and README.md carefully.")
    );
}

#[test]
fn materializer_persists_retry_delta_summary_and_rescue_card_section() -> TestResult {
    let mut input = sample_input(ExecutionContract::RetryRescue);
    input.rescue_card = Some("Avoid replaying the repeated parse failure.".to_owned());
    input.retry_delta_summary =
        Some("Changed retry framing: use a different verification path.".to_owned());

    let materialized = materialize_prompt(input);

    assert_eq!(
        materialized.manifest.retry_delta_summary.as_deref(),
        Some("Changed retry framing: use a different verification path.")
    );
    let rescue_card = materialized
        .manifest
        .sections
        .iter()
        .find(|section| section.kind == PromptSegmentKind::RescueCard)
        .ok_or("missing rescue-card section")?;
    assert!(rescue_card.included);
    assert!(
        materialized
            .rendered_prompt
            .contains("Avoid replaying the repeated parse failure.")
    );
    Ok(())
}

#[test]
fn manifest_tracks_checkpoint_provenance() -> TestResult {
    let materialized = materialize_prompt(sample_input(ExecutionContract::Resume));
    let checkpoint = materialized
        .manifest
        .sections
        .iter()
        .find(|section| section.kind == PromptSegmentKind::Checkpoint)
        .ok_or("missing checkpoint section")?;

    assert_eq!(
        checkpoint
            .provenance
            .checkpoint_id
            .as_ref()
            .map(|id| id.as_str()),
        Some("chk-1")
    );
    assert!(materialized.manifest.estimated_tokens > 0);
    Ok(())
}

#[test]
fn preview_text_handles_multibyte_characters() {
    let text = format!(
        "{}{}",
        "a".repeat(79),
        "│     → <b>extract</b> lessons into playbook"
    );

    let preview = super::preview_text(&text);

    assert!(preview.ends_with('…'));
    assert_eq!(preview.chars().count(), 81);
    assert!(preview.is_char_boundary(preview.len()));
}
