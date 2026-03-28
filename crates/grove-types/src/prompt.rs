use crate::{
    BeadId, BulletId, CheckpointId, EscalationTier, MutationStrategy, PromptId, RunId, SessionId,
    SourceId, Timestamp,
};
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
    StartupPrompt,
    Task,
    Reservation,
    ParentHandoff,
    Checkpoint,
    Playbook,
    ArchiveSnippet,
    Protocol,
    Contract,
    RescueCard,
    EscalationContext,
}

impl PromptSegmentKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StartupPrompt => "startup_prompt",
            Self::Task => "task",
            Self::Reservation => "reservation",
            Self::ParentHandoff => "parent_handoff",
            Self::Checkpoint => "checkpoint",
            Self::Playbook => "playbook",
            Self::ArchiveSnippet => "archive_snippet",
            Self::Protocol => "protocol",
            Self::Contract => "contract",
            Self::RescueCard => "rescue_card",
            Self::EscalationContext => "escalation_context",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscalationContext {
    pub tier: EscalationTier,
    pub mutation_strategy: Option<MutationStrategy>,
    pub tier_number: u32,
    pub is_terminal: bool,
    pub instruction: String,
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
mod tests;
