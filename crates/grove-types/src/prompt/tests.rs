
use super::{
    ExecutionContract, PromptManifest, PromptManifestSection, PromptSectionProvenance,
    PromptSegment, PromptSegmentKind, PromptTrimReason,
};
use crate::{BeadId, BulletId, CheckpointId, PromptId, RunId, SessionId, SourceId, Timestamp};
use std::error::Error;

type TestResult = Result<(), Box<dyn Error>>;

#[test]
fn execution_contract_roundtrips_via_serde() -> TestResult {
    let encoded = serde_json::to_string(&ExecutionContract::RetryRescue)?;
    let decoded: ExecutionContract = serde_json::from_str(&encoded)?;
    assert_eq!(decoded, ExecutionContract::RetryRescue);
    Ok(())
}

#[test]
fn prompt_segment_roundtrips_via_serde() -> TestResult {
    let segment = PromptSegment {
        kind: PromptSegmentKind::Playbook,
        priority: 40,
        heading: "Playbook bullet".to_owned(),
        text: "Prefer explicit exit markers.".to_owned(),
        estimated_tokens: 9,
        provenance: PromptSectionProvenance {
            bullet_ids: vec![BulletId::new("bullet-1")],
            ..PromptSectionProvenance::default()
        },
    };

    let encoded = serde_json::to_string(&segment)?;
    let decoded: PromptSegment = serde_json::from_str(&encoded)?;
    assert_eq!(decoded, segment);
    Ok(())
}

#[test]
fn prompt_manifest_roundtrips_via_serde() -> TestResult {
    let created_at: Timestamp = "2026-03-18T00:00:00Z".parse()?;
    let manifest = PromptManifest {
        prompt_id: PromptId::new("prompt-1"),
        bead_id: BeadId::new("grove-1"),
        run_id: RunId::new("run-1"),
        session_id: Some(SessionId::new("ses-1")),
        contract: ExecutionContract::Implement,
        created_at,
        token_budget: Some(120),
        estimated_tokens: 91,
        prompt_bytes: 420,
        trimmed: true,
        retry_delta_summary: Some("dropped older handoff on retry".to_owned()),
        retrieval_query: Some("auth middleware".to_owned()),
        retrieval_ranking_summary: vec!["src-1 > src-2".to_owned()],
        sections: vec![PromptManifestSection {
            ordinal: 1,
            kind: PromptSegmentKind::Checkpoint,
            heading: "Latest checkpoint".to_owned(),
            included: true,
            estimated_tokens: 14,
            char_count: 56,
            trim_reason: Some(PromptTrimReason::VerboseParentHandoff),
            provenance: PromptSectionProvenance {
                source_ids: vec![SourceId::new("src-1")],
                bullet_ids: vec![BulletId::new("bullet-1")],
                checkpoint_id: Some(CheckpointId::new("chk-1")),
                handoff_run_id: Some(RunId::new("run-parent")),
                archive_message_id: None,
                playbook_bullet_id: None,
            },
            preview: "Progress: routes done".to_owned(),
        }],
    };

    let encoded = serde_json::to_string(&manifest)?;
    let decoded: PromptManifest = serde_json::from_str(&encoded)?;
    assert_eq!(decoded, manifest);
    Ok(())
}
