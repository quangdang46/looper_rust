//! Outcome repository — per-execution result tracking with trend queries.
//!
//! Inspired by ContribAI's per-repo outcome learning with SQLite + TTL aging.
//! Each loop execution records its outcome (success/fail/timeout) for trend
//! analysis and continuous improvement.

use std::collections::HashMap;
use std::sync::Arc;

use rusqlite::Connection;

use crate::error::Result;
use crate::record::OutcomeRecord;

fn scan_outcome_row(row: &rusqlite::Row) -> rusqlite::Result<OutcomeRecord> {
    Ok(OutcomeRecord {
        id: row.get("id")?,
        loop_id: row.get("loop_id")?,
        run_id: row.get("run_id")?,
        project_id: row.get("project_id")?,
        repo: row.get("repo")?,
        loop_type: row.get("loop_type")?,
        status: row.get("status")?,
        duration_ms: row.get("duration_ms")?,
        exit_code: row.get("exit_code")?,
        output_hash: row.get("output_hash")?,
        error_message: row.get("error_message")?,
        error_kind: row.get("error_kind")?,
        metadata_json: row.get("metadata_json")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

const OUTCOME_COLUMNS: &str =
    "id, loop_id, run_id, project_id, repo, loop_type, status, duration_ms, exit_code, output_hash, error_message, error_kind, metadata_json, created_at, updated_at";

#[derive(Clone)]
pub struct OutcomesRepository {
    conn: Arc<Connection>,
}

impl OutcomesRepository {
    pub fn new(conn: Arc<Connection>) -> Self {
        Self { conn }
    }

    /// Insert a new outcome record.
    pub fn insert(&self, record: &OutcomeRecord) -> Result<()> {
        self.conn.execute(
            "INSERT INTO outcomes (id, loop_id, run_id, project_id, repo, loop_type, status, duration_ms, exit_code, output_hash, error_message, error_kind, metadata_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            rusqlite::params![
                &record.id,
                &record.loop_id,
                &record.run_id,
                &record.project_id,
                &record.repo,
                &record.loop_type,
                &record.status,
                &record.duration_ms,
                &record.exit_code,
                &record.output_hash,
                &record.error_message,
                &record.error_kind,
                &record.metadata_json,
                &record.created_at,
                &record.updated_at,
            ],
        )?;
        Ok(())
    }

    /// Get the latest outcome for a specific loop.
    pub fn get_latest_for_loop(&self, loop_id: &str) -> Result<Option<OutcomeRecord>> {
        let sql =
            format!("SELECT {} FROM outcomes WHERE loop_id = ?1 ORDER BY created_at DESC LIMIT 1", OUTCOME_COLUMNS);
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query_map(rusqlite::params![loop_id], scan_outcome_row)?;
        match rows.next() {
            Some(Ok(record)) => Ok(Some(record)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// Get trend summary: how many successes/failures per loop_type for a project.
    ///
    /// Returns a map of `loop_type → (status → count)`.
    pub fn trend_by_project(&self, project_id: &str, limit_days: i64) -> Result<HashMap<String, HashMap<String, i64>>> {
        let mut stmt = self.conn.prepare(
            "SELECT loop_type, status, COUNT(*) as cnt FROM outcomes
             WHERE project_id = ?1 AND created_at >= datetime('now', ?2)
             GROUP BY loop_type, status",
        )?;
        let since = format!("-{limit_days} days");
        let rows = stmt.query_map(rusqlite::params![project_id, since], |row| {
            let loop_type: String = row.get("loop_type")?;
            let status: String = row.get("status")?;
            let cnt: i64 = row.get("cnt")?;
            Ok((loop_type, status, cnt))
        })?;

        let mut map: HashMap<String, HashMap<String, i64>> = HashMap::new();
        for row in rows {
            let (loop_type, status, cnt) = row?;
            map.entry(loop_type).or_default().insert(status, cnt);
        }
        Ok(map)
    }

    /// Get the failure rate (0.0–1.0) for a specific loop_type in a project.
    pub fn failure_rate(&self, project_id: &str, loop_type: &str, limit_days: i64) -> Result<f64> {
        let since = format!("-{limit_days} days");
        let (total, failed): (i64, i64) = self.conn.query_row(
            "SELECT
                COUNT(*) as total,
                SUM(CASE WHEN status IN ('failed', 'timeout') THEN 1 ELSE 0 END) as failed
             FROM outcomes
             WHERE project_id = ?1 AND loop_type = ?2 AND created_at >= datetime('now', ?3)",
            rusqlite::params![project_id, loop_type, since],
            |row| {
                let total: i64 = row.get("total")?;
                let failed: i64 = row.get("failed")?;
                Ok((total, failed))
            },
        )?;

        if total == 0 {
            return Ok(0.0);
        }
        Ok(failed as f64 / total as f64)
    }

    /// Find duplicate outcomes by output_hash (same output = likely same issue).
    pub fn find_duplicates(&self, output_hash: &str) -> Result<Vec<OutcomeRecord>> {
        let sql = format!("SELECT {} FROM outcomes WHERE output_hash = ?1 ORDER BY created_at DESC", OUTCOME_COLUMNS);
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params![output_hash], scan_outcome_row)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }

    /// Delete outcomes older than N days (TTL aging).
    pub fn delete_older_than(&self, days: i64) -> Result<u64> {
        let cutoff = format!("-{days} days");
        let deleted = self
            .conn
            .execute("DELETE FROM outcomes WHERE created_at < datetime('now', ?1)", rusqlite::params![cutoff])?;
        Ok(deleted as u64)
    }
}
