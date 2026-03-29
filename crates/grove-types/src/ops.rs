use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{BeadId, RunId, SessionId};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptMaterializationRecord {
    pub id: String,
    pub bead_id: BeadId,
    pub run_id: RunId,
    pub session_id: SessionId,
    pub kind: String,
    pub prompt_path: String,
    pub prompt_hash: String,
    pub byte_count: i32,
    pub segment_manifest_json: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchDecisionRecord {
    pub id: String,
    pub bead_id: BeadId,
    pub tick_id: String,
    pub disposition: String,
    pub score_breakdown_json: String,
    pub blocking_reasons_json: String,
    pub competing_bead_ids_json: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigSnapshotRecord {
    pub id: String,
    pub sha256: String,
    pub source_path: String,
    pub config_json: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrityCheckRecord {
    pub id: String,
    pub scope: String,
    pub scope_key: Option<String>,
    pub status: String,
    pub findings_json: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanupSnapshotRecord {
    pub id: String,
    pub bead_id: BeadId,
    pub run_id: RunId,
    pub session_id: SessionId,
    pub provider: String,
    pub model: String,
    pub cleaned_artifact_paths: Vec<String>,
    pub cleaned_artifact_kinds: Vec<String>,
    pub deleted_bytes: i64,
    pub continuity_summary: String,
    pub next_bead_guidance: String,
    pub lessons: Vec<String>,
    pub decisions: Vec<String>,
    pub warnings: Vec<String>,
    pub prompt_summary: String,
    pub transcript_tail_summary: String,
    pub created_at: DateTime<Utc>,
}
