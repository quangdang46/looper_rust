use std::collections::HashMap;
use std::sync::Arc;

use crate::error::Result;
use crate::helpers::{chunk_strings, sql_placeholders, SQLITE_MAX_VARIABLES};
use crate::record::{RunRecord, StatusCountMap};
use rusqlite::Connection;

fn scan_run_row(row: &rusqlite::Row) -> rusqlite::Result<RunRecord> {
    Ok(RunRecord {
        id: row.get("id")?,
        loop_id: row.get("loop_id")?,
        status: row.get("status")?,
        current_step: row.get("current_step")?,
        last_completed_step: row.get("last_completed_step")?,
        checkpoint_json: row.get("checkpoint_json")?,
        summary: row.get("summary")?,
        error_message: row.get("error_message")?,
        agent_vendor: row.get("agent_vendor")?,
        model: row.get("model")?,
        started_at: row.get("started_at")?,
        last_heartbeat_at: row.get("last_heartbeat_at")?,
        ended_at: row.get("ended_at")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

const RUN_COLUMNS: &str =
    "id, loop_id, status, current_step, last_completed_step, checkpoint_json, summary, error_message, agent_vendor, model, started_at, last_heartbeat_at, ended_at, created_at, updated_at";

#[derive(Clone)]
pub struct RunsRepository {
    conn: Arc<Connection>,
}

impl RunsRepository {
    pub fn new(conn: Arc<Connection>) -> Self {
        Self { conn }
    }

    pub fn upsert(&self, record: &RunRecord) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO runs (id, loop_id, status, current_step, last_completed_step, checkpoint_json, summary, error_message, agent_vendor, model, started_at, last_heartbeat_at, ended_at, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            rusqlite::params![
                &record.id,
                &record.loop_id,
                &record.status,
                &record.current_step,
                &record.last_completed_step,
                &record.checkpoint_json,
                &record.summary,
                &record.error_message,
                &record.agent_vendor,
                &record.model,
                &record.started_at,
                &record.last_heartbeat_at,
                &record.ended_at,
                &record.created_at,
                &record.updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn get_by_id(&self, id: &str) -> Result<Option<RunRecord>> {
        let sql = format!("SELECT {} FROM runs WHERE id = ?1", RUN_COLUMNS);
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query_map(rusqlite::params![id], scan_run_row)?;
        match rows.next() {
            Some(Ok(record)) => Ok(Some(record)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    pub fn get_latest_by_loop_id(&self, loop_id: &str) -> Result<Option<RunRecord>> {
        let sql = format!("SELECT {} FROM runs WHERE loop_id = ?1 ORDER BY created_at DESC LIMIT 1", RUN_COLUMNS);
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query_map(rusqlite::params![loop_id], scan_run_row)?;
        match rows.next() {
            Some(Ok(record)) => Ok(Some(record)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    pub fn list_latest_by_loop_ids(&self, loop_ids: &[String]) -> Result<Vec<RunRecord>> {
        if loop_ids.is_empty() {
            return Ok(Vec::new());
        }
        let chunks = chunk_strings(loop_ids, SQLITE_MAX_VARIABLES);
        let mut results = Vec::new();
        for chunk in &chunks {
            let inside = sql_placeholders(chunk.len());
            let sql = format!(
                "SELECT r.* FROM runs r INNER JOIN (
                    SELECT loop_id, MAX(created_at) as max_created
                    FROM runs WHERE loop_id IN ({})
                    GROUP BY loop_id
                ) latest ON r.loop_id = latest.loop_id AND r.created_at = latest.max_created",
                inside,
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter().map(|s| s.as_str())), scan_run_row)?;
            for row in rows {
                results.push(row?);
            }
        }
        Ok(results)
    }

    pub fn list_latest_by_loop_statuses_and_resume_policy(
        &self,
        statuses: &[String],
        resume_policy: &str,
    ) -> Result<Vec<RunRecord>> {
        if statuses.is_empty() {
            return Ok(Vec::new());
        }
        let chunks = chunk_strings(statuses, SQLITE_MAX_VARIABLES);
        let mut results = Vec::new();
        for chunk in &chunks {
            let inside = sql_placeholders(chunk.len());
            let like_pattern = format!("%\"resume_policy\":\"{}%", resume_policy);
            let sql = format!(
                "SELECT r.* FROM runs r
                 JOIN loops l ON l.id = r.loop_id
                 WHERE l.status IN ({})
                 AND r.checkpoint_json LIKE ?{} ESCAPE ''",
                inside,
                chunk.len() + 1,
            );
            let mut stmt = self.conn.prepare(&sql)?;
            for (i, status) in chunk.iter().enumerate() {
                stmt.raw_bind_parameter(i + 1, status.as_str())?;
            }
            stmt.raw_bind_parameter(chunk.len() + 1, &like_pattern)?;
            let rows = stmt.query_map([], scan_run_row)?;
            for row in rows {
                results.push(row?);
            }
        }
        Ok(results)
    }

    pub fn has_running_by_loop_id(&self, loop_id: &str) -> Result<bool> {
        let mut stmt =
            self.conn.prepare("SELECT EXISTS(SELECT 1 FROM runs WHERE loop_id = ?1 AND status = 'running')")?;
        let result: bool = stmt.query_row(rusqlite::params![loop_id], |row| row.get(0))?;
        Ok(result)
    }

    pub fn list(&self) -> Result<Vec<RunRecord>> {
        let sql = format!("SELECT {} FROM runs ORDER BY created_at DESC", RUN_COLUMNS);
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], scan_run_row)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }

    pub fn count_by_status(&self) -> Result<StatusCountMap> {
        let mut stmt = self.conn.prepare("SELECT status, COUNT(*) as cnt FROM runs GROUP BY status")?;
        let rows = stmt.query_map([], |row| {
            let status: String = row.get("status")?;
            let cnt: i64 = row.get("cnt")?;
            Ok((status, cnt))
        })?;
        let mut map: StatusCountMap = HashMap::new();
        for row in rows {
            let (status, cnt) = row?;
            map.insert(status, cnt);
        }
        Ok(map)
    }

    pub fn list_since(&self, since_iso: &str) -> Result<Vec<RunRecord>> {
        let sql = format!("SELECT {} FROM runs WHERE created_at >= ?1 ORDER BY created_at DESC", RUN_COLUMNS);
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params![since_iso], scan_run_row)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }

    pub fn list_by_status(&self, status: &str) -> Result<Vec<RunRecord>> {
        let sql = format!("SELECT {} FROM runs WHERE status = ?1 ORDER BY created_at DESC", RUN_COLUMNS);
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params![status], scan_run_row)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }

    pub fn list_by_loop(&self, loop_id: &str) -> Result<Vec<RunRecord>> {
        let sql = format!("SELECT {} FROM runs WHERE loop_id = ?1 ORDER BY created_at DESC", RUN_COLUMNS);
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params![loop_id], scan_run_row)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }
}
