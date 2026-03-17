use crate::{BeadId, CheckpointId, SessionId, Timestamp};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckpointPayload {
    pub progress: String,
    pub next_step: String,
    pub context: Value,
    pub open_questions: Vec<String>,
    pub claimed_paths: Vec<String>,
    pub confidence: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointRecord {
    pub id: CheckpointId,
    pub bead_id: BeadId,
    pub run_id: crate::RunId,
    pub session_id: SessionId,
    pub progress: String,
    pub next_step: String,
    pub payload: Value,
    pub saved_at: Timestamp,
    pub resume_generation: ResumeGeneration,
}

pub type ResumeGeneration = u32;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ProtocolEvent {
    Result { summary: String },
    Artifacts { items: Vec<String> },
    Lessons { items: Vec<String> },
    Decisions { items: Vec<String> },
    Warnings { items: Vec<String> },
    Exit { value: bool },
    Checkpoint { payload: CheckpointPayload },
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ProtocolState {
    pub result_summary: Option<String>,
    pub artifacts: Vec<String>,
    pub lessons: Vec<String>,
    pub decisions: Vec<String>,
    pub warnings: Vec<String>,
    pub explicit_exit: Option<bool>,
    pub latest_checkpoint: Option<CheckpointPayload>,
    pub events: Vec<ProtocolEvent>,
}
