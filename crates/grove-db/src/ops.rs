use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension, Row};
use grove_types::{
    BeadId, RunId, SessionId, ConfigSnapshotRecord, DispatchDecisionRecord, IntegrityCheckRecord,
    PromptMaterializationRecord,
};

use crate::{Database, timestamp_string};

impl Database {
    // 21.2 Prompt materialization
    pub fn insert_prompt_materialization(
        &mut self,
        record: &PromptMaterializationRecord,
    ) -> Result<()> {
        self.with_tx(|tx| {
            tx.execute(
                "INSERT INTO prompt_materializations(\
                    id, bead_id, run_id, session_id, kind, prompt_path, prompt_hash, \
                    byte_count, segment_manifest_json, created_at\
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    record.id,
                    record.bead_id.as_str(),
                    record.run_id.as_str(),
                    record.session_id.as_str(),
                    record.kind,
                    record.prompt_path,
                    record.prompt_hash,
                    record.byte_count,
                    record.segment_manifest_json,
                    timestamp_string(&record.created_at),
                ],
            )?;
            Ok(())
        })
        .with_context(|| format!("insert prompt materialization {}", record.id))
    }

    pub fn list_prompt_materializations_for_bead(
        &self,
        bead_id: &BeadId,
    ) -> Result<Vec<PromptMaterializationRecord>> {
        let mut stmt = self.connection().prepare(
            "SELECT id, bead_id, run_id, session_id, kind, prompt_path, prompt_hash, byte_count, segment_manifest_json, created_at \
             FROM prompt_materializations \
             WHERE bead_id = ?1 \
             ORDER BY created_at DESC"
        )?;

        let rows = stmt.query_map([bead_id.as_str()], |row| {
            Ok(PromptMaterializationRecord {
                id: row.get(0)?,
                bead_id: BeadId::new(row.get::<_, String>(1)?),
                run_id: RunId::new(row.get::<_, String>(2)?),
                session_id: SessionId::new(row.get::<_, String>(3)?),
                kind: row.get(4)?,
                prompt_path: row.get(5)?,
                prompt_hash: row.get(6)?,
                byte_count: row.get(7)?,
                segment_manifest_json: row.get(8)?,
                created_at: row.get::<_, String>(9)?.parse().map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, 0))?, // simplistic error mapping
            })
        })?;

        let records = rows.collect::<Result<Vec<_>, _>>().context("fetch prompt materializations")?;
        Ok(records)
    }

    // 21.3 Dispatch decision
    pub fn insert_dispatch_decision(
        &mut self,
        record: &DispatchDecisionRecord,
    ) -> Result<()> {
        self.with_tx(|tx| {
            tx.execute(
                "INSERT INTO dispatch_decisions(\
                    id, bead_id, tick_id, disposition, score_breakdown_json, \
                    blocking_reasons_json, competing_bead_ids_json, created_at\
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    record.id,
                    record.bead_id.as_str(),
                    record.tick_id,
                    record.disposition,
                    record.score_breakdown_json,
                    record.blocking_reasons_json,
                    record.competing_bead_ids_json,
                    timestamp_string(&record.created_at),
                ],
            )?;
            Ok(())
        })
        .with_context(|| format!("insert dispatch decision {}", record.id))
    }

    pub fn list_dispatch_decisions_for_bead(
        &self,
        bead_id: &BeadId,
        limit: usize,
    ) -> Result<Vec<DispatchDecisionRecord>> {
        let mut stmt = self.connection().prepare(
            "SELECT id, bead_id, tick_id, disposition, score_breakdown_json, blocking_reasons_json, competing_bead_ids_json, created_at \
             FROM dispatch_decisions \
             WHERE bead_id = ?1 \
             ORDER BY created_at DESC \
             LIMIT ?2"
        )?;

        let rows = stmt.query_map(params![bead_id.as_str(), limit as i64], |row| {
            Ok(DispatchDecisionRecord {
                id: row.get(0)?,
                bead_id: BeadId::new(row.get::<_, String>(1)?),
                tick_id: row.get(2)?,
                disposition: row.get(3)?,
                score_breakdown_json: row.get(4)?,
                blocking_reasons_json: row.get(5)?,
                competing_bead_ids_json: row.get(6)?,
                created_at: row.get::<_, String>(7)?.parse().map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, 0))?,
            })
        })?;

        let records = rows.collect::<Result<Vec<_>, _>>().context("fetch dispatch decisions")?;
        Ok(records)
    }

    // 21.5 Config snapshot
    pub fn insert_config_snapshot(
        &mut self,
        record: &ConfigSnapshotRecord,
    ) -> Result<()> {
        self.with_tx(|tx| {
            tx.execute(
                "INSERT OR IGNORE INTO config_snapshots(\
                    id, sha256, source_path, config_json, created_at\
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    record.id,
                    record.sha256,
                    record.source_path,
                    record.config_json,
                    timestamp_string(&record.created_at),
                ],
            )?;
            Ok(())
        })
        .with_context(|| format!("insert config snapshot {}", record.sha256))
    }

    // 21.6 Integrity checks
    pub fn insert_integrity_check(
        &mut self,
        record: &IntegrityCheckRecord,
    ) -> Result<()> {
        self.with_tx(|tx| {
            tx.execute(
                "INSERT INTO integrity_checks(\
                    id, scope, scope_key, status, findings_json, created_at\
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    record.id,
                    record.scope,
                    record.scope_key.as_deref(),
                    record.status,
                    record.findings_json,
                    timestamp_string(&record.created_at),
                ],
            )?;
            Ok(())
        })
        .with_context(|| format!("insert integrity check {}", record.id))
    }
}
