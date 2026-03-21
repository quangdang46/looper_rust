//! Run diaries and outcome feedback extraction.
//!
//! Implements PLAN.md § 1.2.5 Diary and Outcome Feedback.
//! This module analyzes session outcomes and uses heuristics like duration, error counts,
//! and retries to generate implicit feedback scored as Helpful or Harmful,
//! which is then applied automatically to any Playbook bullets that were active during the session.

use anyhow::{Context, Result};
use chrono::Utc;
use grove_db::Database;
use grove_types::{
    BeadId, BulletId, PromptManifest, PromptSegmentKind, RunId, SessionOutcome, SessionStatus,
    playbook::{FeedbackEventRecord, FeedbackKind},
};
use std::fs;
use std::path::Path;

/// Fast-path extraction of diary metrics from a session outcome.
pub struct DiaryEntry {
    pub session_id: String,
    pub bead_id: Option<BeadId>,
    pub outcome: SessionStatus,
    pub duration_secs: u64,
    pub error_count: u32,
    pub had_retries: bool,
}

impl DiaryEntry {
    /// Score the diary entry into implicit `(helpful, harmful)` weights.
    pub fn score_implicit_feedback(&self) -> (f64, f64) {
        let mut helpful: f64 = 0.0;
        let mut harmful: f64 = 0.0;

        match self.outcome {
            SessionStatus::Completed => helpful += 1.0,
            SessionStatus::Crashed | SessionStatus::UnknownFailure => harmful += 1.0,
            SessionStatus::Checkpointed | SessionStatus::TimedOut => {
                helpful += 0.3;
                harmful += 0.3;
            }
            _ => {}
        }

        if self.duration_secs < 600 {
            helpful += 0.5;
        }
        if self.duration_secs > 3600 {
            harmful += 0.3;
        }
        if self.error_count > 0 {
            harmful += 0.2 * f64::from(self.error_count.min(5));
        }
        if self.had_retries {
            harmful += 0.3;
        }

        (helpful.clamp(0.0, 2.0), harmful.clamp(0.0, 2.0))
    }
}

/// Extract implicit outcome feedback from a completed session and apply it to
/// all active bullets injected during its materialization phase.
pub fn apply_outcome_feedback(
    db: &mut Database,
    bead_id: &BeadId,
    run_id: &RunId,
    outcome: &SessionOutcome,
    had_retries: bool,
) -> Result<()> {
    // Determine duration
    let duration_secs = if let Some(ended) = outcome.session.ended_at {
        (ended - outcome.session.started_at)
            .num_seconds()
            .max(0) as u64
    } else {
        0
    };

    let diary = DiaryEntry {
        session_id: outcome.session.id.as_str().to_string(),
        bead_id: Some(bead_id.clone()),
        outcome: outcome.session.status,
        duration_secs,
        // Count errors from standard stdout/stderr analysis and protocol warnings
        error_count: outcome.analysis.warnings.len() as u32
            + outcome.analysis.permission_denials
            + outcome.analysis.rate_limit_markers,
        had_retries,
    };

    let (helpful_weight, harmful_weight) = diary.score_implicit_feedback();

    // Read the prompt manifest to figure out which bullets were applied.
    let manifest_path = outcome
        .session
        .prompt_manifest_path
        .as_deref()
        .unwrap_or_default();
    if manifest_path.is_empty() || !Path::new(manifest_path).exists() {
        // No manifest saved, can't apply implicit feedback to bullets
        return Ok(());
    }

    let manifest_json = fs::read_to_string(manifest_path)
        .with_context(|| format!("read prompt manifest at {manifest_path}"))?;
    let manifest: PromptManifest = serde_json::from_str(&manifest_json)
        .with_context(|| format!("parse prompt manifest at {manifest_path}"))?;

    // Find injected playbook bullets
    let mut active_bullet_ids: Vec<BulletId> = Vec::new();
    for section in manifest.sections {
        if section.kind == PromptSegmentKind::Playbook && section.included {
            active_bullet_ids.extend(section.provenance.bullet_ids.into_iter());
        }
    }

    let now = Utc::now();
    let context_msg = format!("Implicit diary feedback (Duration: {}s, Errors: {})", diary.duration_secs, diary.error_count);

    for bullet_id in active_bullet_ids {
        if helpful_weight > 0.0 {
            let event = FeedbackEventRecord {
                kind: FeedbackKind::Helpful,
                timestamp: now,
                bead_id: Some(bead_id.clone()),
                run_id: Some(run_id.clone()),
                context: Some(context_msg.clone()),
                weight: helpful_weight as f32,
            };
            let _ = db.record_playbook_feedback(&bullet_id, &event);
        }

        if harmful_weight > 0.0 {
            let event = FeedbackEventRecord {
                kind: FeedbackKind::Harmful,
                timestamp: now,
                bead_id: Some(bead_id.clone()),
                run_id: Some(run_id.clone()),
                context: Some(context_msg.clone()),
                weight: harmful_weight as f32,
            };
            let _ = db.record_playbook_feedback(&bullet_id, &event);
        }
    }

    Ok(())
}
