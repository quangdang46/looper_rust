//! Lesson ingestion — extracts candidate playbook bullets from handoff lessons
//! and explicit GROVE_LESSONS protocol markers, then inserts them as draft bullets.

use anyhow::Result;
use grove_db::Database;
use grove_types::{
    BeadId, BulletId, RunId,
    playbook::{
        BulletMaturity, BulletScope, BulletState, BulletType, FeedbackEventRecord, FeedbackKind,
        PlaybookBulletRecord,
    },
};

/// Ingest lesson strings from handoff or protocol output into the playbook as draft candidates.
///
/// Returns the number of new bullets created (duplicates are reinforced instead).
pub fn ingest_lessons(
    db: &mut Database,
    bead_id: &BeadId,
    run_id: &RunId,
    lessons: &[String],
) -> Result<usize> {
    let now = chrono::Utc::now();
    let mut created = 0;

    for lesson in lessons {
        let trimmed = lesson.trim();
        if trimmed.is_empty() {
            continue;
        }

        let hash = content_hash(trimmed);

        // Check for exact duplicate (content hash)
        if let Some(existing_id) = db.find_bullet_by_hash(&hash)? {
            // Reinforce existing bullet with Helpful feedback
            db.record_playbook_feedback(
                &existing_id,
                &FeedbackEventRecord {
                    kind: FeedbackKind::Helpful,
                    timestamp: now,
                    bead_id: Some(bead_id.clone()),
                    run_id: Some(run_id.clone()),
                    context: Some("reinforced by repeated lesson".to_string()),
                    weight: 1.0,
                },
            )?;
            db.log_curation_action(
                &existing_id,
                "reinforce",
                Some("duplicate lesson reinforced via ingest"),
                None,
                None,
                None,
                None,
            )?;
            continue;
        }

        // Insert as new draft bullet
        let bullet_id = BulletId::new(format!("blt-{}-{}", run_id.as_str(), created));
        let bullet = PlaybookBulletRecord {
            id: bullet_id.clone(),
            scope: BulletScope::Global,
            scope_key: None,
            category: "session_lesson".to_string(),
            text: trimmed.to_string(),
            bullet_type: classify_bullet_type(trimmed),
            state: BulletState::Draft,
            maturity: BulletMaturity::Candidate,
            helpful_count: 1,
            harmful_count: 0,
            feedback_events: vec![],
            confidence_decay_half_life_days: 30,
            pinned: false,
            deprecated: false,
            replaced_by: None,
            deprecation_reason: None,
            source_bead_ids: vec![bead_id.clone()],
            source_run_ids: vec![run_id.clone()],
            tags: vec![],
            effective_score: Some(1.0),
            created_at: now,
            updated_at: now,
        };

        db.insert_playbook_bullet(&bullet)?;

        // Initial helpful feedback from the originating session
        db.record_playbook_feedback(
            &bullet_id,
            &FeedbackEventRecord {
                kind: FeedbackKind::Helpful,
                timestamp: now,
                bead_id: Some(bead_id.clone()),
                run_id: Some(run_id.clone()),
                context: Some("initial ingestion from session lesson".to_string()),
                weight: 1.0,
            },
        )?;

        db.log_curation_action(
            &bullet_id,
            "add",
            Some(&format!(
                "ingested from bead {} run {}",
                bead_id.as_str(),
                run_id.as_str()
            )),
            None,
            Some("draft"),
            None,
            Some("candidate"),
        )?;

        created += 1;
    }

    Ok(created)
}

/// Simple heuristic: if the lesson contains avoidance language, classify as AntiPattern.
fn classify_bullet_type(text: &str) -> BulletType {
    let lower = text.to_lowercase();
    let anti_markers = ["avoid", "don't", "do not", "never", "stop", "remove"];
    if anti_markers.iter().any(|m| lower.contains(m)) {
        BulletType::AntiPattern
    } else {
        BulletType::Rule
    }
}

fn content_hash(text: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let normalized = text.trim().to_lowercase();
    let mut hasher = DefaultHasher::new();
    normalized.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}
