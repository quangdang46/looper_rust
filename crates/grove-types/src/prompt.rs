use crate::{BeadId, BulletId, CheckpointId, PromptId, RunId, SessionId, SourceId, Timestamp};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionContract {
    Implement,
    Resume,
    RetryRescue,
    SingleTask,
}

impl ExecutionContract {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Implement => "implement",
            Self::Resume => "resume",
            Self::RetryRescue => "retry_rescue",
            Self::SingleTask => "single_task",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PromptSegmentKind {
    Task,
    Reservation,
    ParentHandoff,
    Checkpoint,
    Playbook,
    ArchiveSnippet,
    Protocol,
    Contract,
    RescueCard,
}

impl PromptSegmentKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Task => "task",
            Self::Reservation => "reservation",
            Self::ParentHandoff => "parent_handoff",
            Self::Checkpoint => "checkpoint",
            Self::Playbook => "playbook",
            Self::ArchiveSnippet => "archive_snippet",
            Self::Protocol => "protocol",
            Self::Contract => "contract",
            Self::RescueCard => "rescue_card",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PromptTrimReason {
    LowerPriorityArchiveSnippet,
    LowerPriorityPlaybookBullet,
    VerboseParentHandoff,
    NonEssentialReservationHint,
}

impl PromptTrimReason {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LowerPriorityArchiveSnippet => "lower_priority_archive_snippet",
            Self::LowerPriorityPlaybookBullet => "lower_priority_playbook_bullet",
            Self::VerboseParentHandoff => "verbose_parent_handoff",
            Self::NonEssentialReservationHint => "non_essential_reservation_hint",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PromptSectionProvenance {
    pub source_ids: Vec<SourceId>,
    pub bullet_ids: Vec<BulletId>,
    pub checkpoint_id: Option<CheckpointId>,
    pub handoff_run_id: Option<RunId>,
    pub archive_message_id: Option<String>,
    pub playbook_bullet_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptSegment {
    pub kind: PromptSegmentKind,
    pub priority: u8,
    pub heading: String,
    pub text: String,
    pub estimated_tokens: u32,
    pub provenance: PromptSectionProvenance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptManifestSection {
    pub ordinal: u32,
    pub kind: PromptSegmentKind,
    pub heading: String,
    pub included: bool,
    pub estimated_tokens: u32,
    pub char_count: u32,
    pub trim_reason: Option<PromptTrimReason>,
    pub provenance: PromptSectionProvenance,
    pub preview: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptManifest {
    pub prompt_id: PromptId,
    pub bead_id: BeadId,
    pub run_id: RunId,
    pub session_id: Option<SessionId>,
    pub contract: ExecutionContract,
    pub created_at: Timestamp,
    pub token_budget: Option<u32>,
    pub estimated_tokens: u32,
    pub prompt_bytes: u32,
    pub trimmed: bool,
    pub retry_delta_summary: Option<String>,
    pub retrieval_query: Option<String>,
    pub retrieval_ranking_summary: Vec<String>,
    pub sections: Vec<PromptManifestSection>,
}

#[cfg(test)]
mod tests {
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
}
