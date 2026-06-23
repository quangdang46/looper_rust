use std::collections::HashMap;
use std::sync::Arc;

use rusqlite::Connection;
use crate::error::Result;
use crate::helpers::{chunk_strings, sql_placeholders, SQLITE_MAX_VARIABLES};
use crate::record::{LoopRecord, TypeStatusCountMap};

fn scan_loop_row(row: &rusqlite::Row) -> rusqlite::Result<LoopRecord> {
    Ok(LoopRecord {
        id: row.get("id")?,
        seq: row.get("seq")?,
        project_id: row.get("project_id")?,
        r#type: row.get("type")?,
        target_type: row.get("target_type")?,
        target_id: row.get("target_id")?,
        repo: row.get("repo")?,
        pr_number: row.get("pr_number")?,
        status: row.get("status")?,
        config_json: row.get("config_json")?,
        metadata_json: row.get("metadata_json")?,
        last_run_at: row.get("last_run_at")?,
        next_run_at: row.get("next_run_at")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

const LOOP_COLUMNS: &str = "id, seq, project_id, type, target_type, target_id, repo, pr_number, status, config_json, metadata_json, last_run_at, next_run_at, created_at, updated_at";

#[derive(Clone)]
pub struct LoopsRepository {
    conn: Arc<Connection>,
}

impl LoopsRepository {
    pub fn new(conn: Arc<Connection>) -> Self {
        Self { conn }
    }

    pub fn upsert(&self, record: &LoopRecord) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO loops (id, seq, project_id, type, target_type, target_id, repo, pr_number, status, config_json, metadata_json, last_run_at, next_run_at, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            rusqlite::params![
                &record.id,
                &record.seq,
                &record.project_id,
                &record.r#type,
                &record.target_type,
                &record.target_id,
                &record.repo,
                &record.pr_number,
                &record.status,
                &record.config_json,
                &record.metadata_json,
                &record.last_run_at,
                &record.next_run_at,
                &record.created_at,
                &record.updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn get_by_id(&self, id: &str) -> Result<Option<LoopRecord>> {
        let sql = format!("SELECT {} FROM loops WHERE id = ?1", LOOP_COLUMNS);
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query_map(rusqlite::params![id], scan_loop_row)?;
        match rows.next() {
            Some(Ok(record)) => Ok(Some(record)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    pub fn get_by_seq(&self, seq: i64) -> Result<Option<LoopRecord>> {
        let sql = format!("SELECT {} FROM loops WHERE seq = ?1", LOOP_COLUMNS);
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query_map(rusqlite::params![seq], scan_loop_row)?;
        match rows.next() {
            Some(Ok(record)) => Ok(Some(record)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    pub fn allocate_seq(&self) -> Result<i64> {
        let result: i64 = self.conn.query_row(
            "INSERT INTO counters (name, value) VALUES ('loop_seq', 1)
             ON CONFLICT(name) DO UPDATE SET value = value + 1
             RETURNING value",
            [],
            |row| row.get("value"),
        )?;
        Ok(result)
    }

    pub fn list(&self) -> Result<Vec<LoopRecord>> {
        let sql = format!("SELECT {} FROM loops ORDER BY seq ASC", LOOP_COLUMNS);
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], scan_loop_row)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }

    pub fn list_by_statuses(&self, statuses: &[String]) -> Result<Vec<LoopRecord>> {
        if statuses.is_empty() {
            return Ok(Vec::new());
        }
        let chunks = chunk_strings(statuses, SQLITE_MAX_VARIABLES);
        let mut results = Vec::new();
        for chunk in &chunks {
            let sql = format!(
                "SELECT {} FROM loops WHERE status IN ({})",
                LOOP_COLUMNS,
                sql_placeholders(chunk.len()),
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let rows =
                stmt.query_map(rusqlite::params_from_iter(chunk.iter().map(|s| s.as_str())), scan_loop_row)?;
            for row in rows {
                results.push(row?);
            }
        }
        Ok(results)
    }

    pub fn list_by_ids(&self, ids: &[String]) -> Result<Vec<LoopRecord>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let chunks = chunk_strings(ids, SQLITE_MAX_VARIABLES);
        let mut results = Vec::new();
        for chunk in &chunks {
            let sql = format!(
                "SELECT {} FROM loops WHERE id IN ({})",
                LOOP_COLUMNS,
                sql_placeholders(chunk.len()),
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let rows =
                stmt.query_map(rusqlite::params_from_iter(chunk.iter().map(|s| s.as_str())), scan_loop_row)?;
            for row in rows {
                results.push(row?);
            }
        }
        Ok(results)
    }

    pub fn count_by_type_and_status(&self) -> Result<TypeStatusCountMap> {
        let mut stmt = self.conn.prepare(
            "SELECT type, status, COUNT(*) as cnt FROM loops GROUP BY type, status",
        )?;
        let rows = stmt.query_map([], |row| {
            let r#type: String = row.get("type")?;
            let status: String = row.get("status")?;
            let cnt: i64 = row.get("cnt")?;
            Ok((r#type, status, cnt))
        })?;
        let mut map: TypeStatusCountMap = HashMap::new();
        for row in rows {
            let (r#type, status, cnt) = row?;
            map.entry(r#type).or_default().insert(status, cnt);
        }
        Ok(map)
    }

    pub fn terminate_by_project(&self, project_id: &str, updated_at: &str) -> Result<i64> {
        let rows = self.conn.execute(
            "UPDATE loops SET status = 'terminated', updated_at = ?2
             WHERE project_id = ?1 AND status NOT IN ('completed', 'terminated', 'stopped')",
            rusqlite::params![project_id, updated_at],
        )?;
        Ok(rows as i64)
    }
}
