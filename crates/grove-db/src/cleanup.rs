use anyhow::{Context, Result};
use grove_types::{BeadId, CleanupSnapshotRecord};
use rusqlite::{OptionalExtension, params};

use crate::{Database, timestamp_string};

impl Database {
    pub fn insert_cleanup_snapshot(&mut self, record: &CleanupSnapshotRecord) -> Result<()> {
        self.with_tx(|tx| {
            tx.execute(
                "INSERT INTO cleanup_snapshots(\
                    id, bead_id, run_id, session_id, provider, model, \
                    cleaned_artifact_paths_json, cleaned_artifact_kinds_json, deleted_bytes, \
                    continuity_summary, next_bead_guidance, lessons_json, decisions_json, \
                    warnings_json, prompt_summary, transcript_tail_summary, created_at\
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
                params![
                    record.id,
                    record.bead_id.as_str(),
                    record.run_id.as_str(),
                    record.session_id.as_str(),
                    record.provider,
                    record.model,
                    serde_json::to_string(&record.cleaned_artifact_paths)?,
                    serde_json::to_string(&record.cleaned_artifact_kinds)?,
                    record.deleted_bytes,
                    record.continuity_summary,
                    record.next_bead_guidance,
                    serde_json::to_string(&record.lessons)?,
                    serde_json::to_string(&record.decisions)?,
                    serde_json::to_string(&record.warnings)?,
                    record.prompt_summary,
                    record.transcript_tail_summary,
                    timestamp_string(&record.created_at),
                ],
            )?;
            Ok(())
        })
        .with_context(|| format!("insert cleanup snapshot {}", record.id))
    }

    pub fn latest_cleanup_snapshot_for_bead(
        &self,
        bead_id: &BeadId,
    ) -> Result<Option<CleanupSnapshotRecord>> {
        let mut stmt = self.connection().prepare(
            "SELECT id, bead_id, run_id, session_id, provider, model, \
                cleaned_artifact_paths_json, cleaned_artifact_kinds_json, deleted_bytes, \
                continuity_summary, next_bead_guidance, lessons_json, decisions_json, \
                warnings_json, prompt_summary, transcript_tail_summary, created_at \
             FROM cleanup_snapshots \
             WHERE bead_id = ?1 \
             ORDER BY created_at DESC, id DESC \
             LIMIT 1",
        )?;

        let record = stmt
            .query_row([bead_id.as_str()], |row| {
                let bead_id = BeadId::new(row.get::<_, String>(1)?);
                let run_id = grove_types::RunId::new(row.get::<_, String>(2)?);
                let session_id = grove_types::SessionId::new(row.get::<_, String>(3)?);
                let created_at = row
                    .get::<_, String>(16)?
                    .parse()
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, 0))?;
                Ok(CleanupSnapshotRecord {
                    id: row.get(0)?,
                    bead_id,
                    run_id,
                    session_id,
                    provider: row.get(4)?,
                    model: row.get(5)?,
                    cleaned_artifact_paths: serde_json::from_str(&row.get::<_, String>(6)?)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, 0))?,
                    cleaned_artifact_kinds: serde_json::from_str(&row.get::<_, String>(7)?)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, 0))?,
                    deleted_bytes: row.get(8)?,
                    continuity_summary: row.get(9)?,
                    next_bead_guidance: row.get(10)?,
                    lessons: serde_json::from_str(&row.get::<_, String>(11)?)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, 0))?,
                    decisions: serde_json::from_str(&row.get::<_, String>(12)?)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, 0))?,
                    warnings: serde_json::from_str(&row.get::<_, String>(13)?)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, 0))?,
                    prompt_summary: row.get(14)?,
                    transcript_tail_summary: row.get(15)?,
                    created_at,
                })
            })
            .optional()?;

        Ok(record)
    }
}
