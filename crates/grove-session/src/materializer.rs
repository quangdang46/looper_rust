#![allow(clippy::unwrap_used, clippy::expect_used)]
pub use grove_types::EscalationContext;
use grove_types::{
    BeadId, CheckpointId, ExecutionContract, PromptId, PromptManifest, PromptManifestSection,
    PromptSectionProvenance, PromptSegment, PromptSegmentKind, PromptTrimReason, RunId, Timestamp,
};

const TOKEN_DIVISOR: usize = 4;

#[derive(Debug, Clone)]
pub struct PromptMaterialization {
    pub prompt_id: PromptId,
    pub contract: ExecutionContract,
    pub rendered_prompt: String,
    pub manifest: PromptManifest,
    pub prompt_bytes: u32,
    pub estimated_input_tokens: u32,
}

#[derive(Debug, Clone)]
pub struct PromptMaterializationInput {
    pub prompt_id: PromptId,
    pub bead_id: BeadId,
    pub run_id: RunId,
    pub created_at: Timestamp,
    pub contract: ExecutionContract,
    pub task_title: String,
    pub task_description: String,
    pub reservation_hints: Vec<String>,
    pub parent_handoffs: Vec<String>,
    pub checkpoint: Option<CheckpointPromptInput>,
    pub protocol_block: String,
    pub rescue_card: Option<String>,
    pub token_budget: Option<u32>,
    pub retry_delta_summary: Option<String>,
    pub retrieval_query: Option<String>,
    pub archive_bundle: Option<grove_types::archive::RetrievalBundle>,
    pub playbook_rules: Vec<grove_types::playbook::PlaybookBulletRecord>,
    pub escalation_context: Option<EscalationContext>,
}

#[derive(Debug, Clone)]
pub struct CheckpointPromptInput {
    pub checkpoint_id: CheckpointId,
    pub progress: String,
    pub next_step: String,
    pub open_questions: Vec<String>,
}

pub fn materialize_prompt(input: PromptMaterializationInput) -> PromptMaterialization {
    let mut sections = vec![build_contract_section(input.contract)];
    sections.push(build_task_section(
        &input.task_title,
        &input.task_description,
    ));

    for reservation in &input.reservation_hints {
        sections.push(build_text_section(
            PromptSegmentKind::Reservation,
            40,
            "Reservation hints",
            reservation,
        ));
    }

    for handoff in &input.parent_handoffs {
        sections.push(build_text_section(
            PromptSegmentKind::ParentHandoff,
            50,
            "Parent handoff",
            handoff,
        ));
    }

    if let Some(archive_bundle) = &input.archive_bundle {
        for snippet in &archive_bundle.snippets {
            let heading = format!("Historical snippet (Score: {:.2})", snippet.score);
            let mut text = String::new();
            if let Some(path) = &snippet.file_path {
                text.push_str(&format!("From {}\n", path));
            }
            text.push_str(&snippet.snippet);
            sections.push(PromptSegment {
                kind: PromptSegmentKind::ArchiveSnippet,
                priority: 30, // lower priority than most standard context
                heading,
                estimated_tokens: estimate_tokens(&text),
                text,
                provenance: grove_types::PromptSectionProvenance {
                    archive_message_id: Some(snippet.message_id.to_string()),
                    ..Default::default()
                },
            });
        }
    }

    for rule in &input.playbook_rules {
        let heading = format!("Playbook {} (Maturity: {:?})", rule.category, rule.maturity);
        let text = format!("[{}] {}", rule.category.to_uppercase(), rule.text);
        sections.push(PromptSegment {
            kind: PromptSegmentKind::Playbook,
            priority: 40, // higher priority than archive snippets, but below direct instructions
            heading,
            estimated_tokens: estimate_tokens(&text),
            text,
            provenance: grove_types::PromptSectionProvenance {
                bullet_ids: vec![rule.id.clone()],
                ..Default::default()
            },
        });
    }

    if let Some(checkpoint) = &input.checkpoint {
        sections.push(build_checkpoint_section(checkpoint));
    }

    if let Some(ref ctx) = input.escalation_context {
        sections.push(build_escalation_context_section(ctx));
    }

    sections.push(build_protocol_section(&input.protocol_block));

    if let Some(rescue_card) = &input.rescue_card {
        sections.push(build_text_section(
            PromptSegmentKind::RescueCard,
            80,
            "Rescue card",
            rescue_card,
        ));
    }

    let materialized = apply_budget(sections, input.token_budget);
    let rendered_prompt = render_sections(&materialized.included_sections);
    let prompt_bytes = rendered_prompt.len() as u32;
    let estimated_input_tokens = estimate_tokens(&rendered_prompt);

    let mut manifest_sections = Vec::with_capacity(materialized.all_sections.len());
    for (ordinal, section) in materialized.all_sections.iter().enumerate() {
        manifest_sections.push(PromptManifestSection {
            ordinal: (ordinal + 1) as u32,
            kind: section.kind,
            heading: section.heading.clone(),
            included: materialized.included_ordinals.contains(&ordinal),
            estimated_tokens: section.estimated_tokens,
            char_count: section.text.len() as u32,
            trim_reason: materialized.trimmed_ordinals.get(&ordinal).copied(),
            provenance: section.provenance.clone(),
            preview: preview_text(&section.text),
        });
    }

    let manifest = PromptManifest {
        prompt_id: input.prompt_id.clone(),
        bead_id: input.bead_id,
        run_id: input.run_id,
        session_id: None,
        contract: input.contract,
        created_at: input.created_at,
        token_budget: input.token_budget,
        estimated_tokens: estimated_input_tokens,
        prompt_bytes,
        trimmed: !materialized.trimmed_ordinals.is_empty(),
        retry_delta_summary: input.retry_delta_summary,
        retrieval_query: input.retrieval_query,
        retrieval_ranking_summary: materialized
            .all_sections
            .iter()
            .filter(|section| section.kind == PromptSegmentKind::ArchiveSnippet)
            .map(|section| section.heading.clone())
            .collect(),
        sections: manifest_sections,
    };

    PromptMaterialization {
        prompt_id: input.prompt_id,
        contract: input.contract,
        rendered_prompt,
        manifest,
        prompt_bytes,
        estimated_input_tokens,
    }
}

#[derive(Debug)]
struct BudgetedSections {
    all_sections: Vec<PromptSegment>,
    included_sections: Vec<PromptSegment>,
    included_ordinals: std::collections::BTreeSet<usize>,
    trimmed_ordinals: std::collections::BTreeMap<usize, PromptTrimReason>,
}

fn apply_budget(sections: Vec<PromptSegment>, token_budget: Option<u32>) -> BudgetedSections {
    let mut included = vec![true; sections.len()];
    let mut trimmed_ordinals = std::collections::BTreeMap::new();

    if let Some(limit) = token_budget {
        let mut current_tokens: u32 = sections
            .iter()
            .map(|section| section.estimated_tokens)
            .sum();

        let trim_order = [
            (
                PromptSegmentKind::ArchiveSnippet,
                PromptTrimReason::LowerPriorityArchiveSnippet,
            ),
            (
                PromptSegmentKind::Playbook,
                PromptTrimReason::LowerPriorityPlaybookBullet,
            ),
            (
                PromptSegmentKind::ParentHandoff,
                PromptTrimReason::VerboseParentHandoff,
            ),
            (
                PromptSegmentKind::Reservation,
                PromptTrimReason::NonEssentialReservationHint,
            ),
        ];

        for (kind, reason) in trim_order {
            if current_tokens <= limit {
                break;
            }

            for index in (0..sections.len()).rev() {
                if current_tokens <= limit {
                    break;
                }
                if included[index] && sections[index].kind == kind {
                    included[index] = false;
                    current_tokens =
                        current_tokens.saturating_sub(sections[index].estimated_tokens);
                    trimmed_ordinals.insert(index, reason);
                }
            }
        }
    }

    let included_sections = sections
        .iter()
        .zip(included.iter())
        .filter(|(_, keep)| **keep)
        .map(|(section, _)| section.clone())
        .collect::<Vec<_>>();
    let included_ordinals = included
        .iter()
        .enumerate()
        .filter_map(|(index, keep)| keep.then_some(index))
        .collect();

    BudgetedSections {
        all_sections: sections,
        included_sections,
        included_ordinals,
        trimmed_ordinals,
    }
}

fn build_contract_section(contract: ExecutionContract) -> PromptSegment {
    let text = match contract {
        ExecutionContract::Implement => {
            "[EXECUTION CONTRACT]\nImplement the requested change directly and keep the scope tight."
        }
        ExecutionContract::Resume => {
            "[EXECUTION CONTRACT]\nResume from the latest checkpoint and continue the in-progress task without redoing completed work."
        }
        ExecutionContract::RetryRescue => {
            "[EXECUTION CONTRACT]\nRetry the task with a changed approach that avoids repeating the previous failure mode."
        }
        ExecutionContract::SingleTask => {
            "[EXECUTION CONTRACT]\nComplete this single Grove task end-to-end and stop when the protocol indicates completion."
        }
    };

    build_text_section(PromptSegmentKind::Contract, 0, "Execution contract", text)
}

fn build_task_section(title: &str, description: &str) -> PromptSegment {
    let text = format!("[TASK]\nTitle: {title}\n\n{description}");
    build_text_section(PromptSegmentKind::Task, 10, "Task", &text)
}

fn build_checkpoint_section(checkpoint: &CheckpointPromptInput) -> PromptSegment {
    let mut text = format!(
        "[CHECKPOINT]\nProgress: {}\nNext step: {}",
        checkpoint.progress, checkpoint.next_step
    );

    if !checkpoint.open_questions.is_empty() {
        text.push_str("\nOpen questions:");
        for question in &checkpoint.open_questions {
            text.push_str("\n- ");
            text.push_str(question);
        }
    }

    PromptSegment {
        kind: PromptSegmentKind::Checkpoint,
        priority: 60,
        heading: "Latest checkpoint".to_owned(),
        estimated_tokens: estimate_tokens(&text),
        text,
        provenance: PromptSectionProvenance {
            checkpoint_id: Some(checkpoint.checkpoint_id.clone()),
            ..PromptSectionProvenance::default()
        },
    }
}

fn build_escalation_context_section(ctx: &EscalationContext) -> PromptSegment {
    PromptSegment {
        kind: PromptSegmentKind::EscalationContext,
        priority: 65,
        heading: format!("Escalation context (tier {})", ctx.tier_number),
        estimated_tokens: estimate_tokens(&ctx.instruction),
        text: format!(
            "[ESCALATION CONTEXT]\nTier: {:?} (attempt {})\n{}\n",
            ctx.tier, ctx.tier_number, ctx.instruction,
        ),
        provenance: PromptSectionProvenance::default(),
    }
}

fn build_protocol_section(protocol_block: &str) -> PromptSegment {
    build_text_section(
        PromptSegmentKind::Protocol,
        70,
        "Grove protocol",
        protocol_block,
    )
}

fn build_text_section(
    kind: PromptSegmentKind,
    priority: u8,
    heading: &str,
    text: &str,
) -> PromptSegment {
    PromptSegment {
        kind,
        priority,
        heading: heading.to_owned(),
        estimated_tokens: estimate_tokens(text),
        text: text.to_owned(),
        provenance: PromptSectionProvenance::default(),
    }
}

fn render_sections(sections: &[PromptSegment]) -> String {
    sections
        .iter()
        .map(|section| section.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn preview_text(text: &str) -> String {
    const MAX_PREVIEW: usize = 80;
    if text.len() <= MAX_PREVIEW {
        text.to_owned()
    } else {
        format!("{}…", &text[..MAX_PREVIEW])
    }
}

fn estimate_tokens(text: &str) -> u32 {
    text.chars().count().div_ceil(TOKEN_DIVISOR) as u32
}

#[cfg(test)]
mod tests {
    use super::{CheckpointPromptInput, PromptMaterializationInput, materialize_prompt};
    use grove_types::{
        BeadId, CheckpointId, ExecutionContract, PromptId, PromptSegmentKind, PromptTrimReason,
        RunId, Timestamp,
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
        let retry = materialize_prompt(sample_input(ExecutionContract::RetryRescue));

        assert!(
            implement
                .rendered_prompt
                .contains("Implement the requested change directly")
        );
        assert!(
            retry
                .rendered_prompt
                .contains("Retry the task with a changed approach")
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
}
