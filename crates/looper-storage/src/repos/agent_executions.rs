use std::sync::Arc;

use crate::error::Result;
use crate::record::AgentExecutionRecord;
use rusqlite::Connection;

fn scan_agent_execution_row(row: &rusqlite::Row) -> rusqlite::Result<AgentExecutionRecord> {
    Ok(AgentExecutionRecord {
        id: row.get("id")?,
        project_id: row.get("project_id")?,
        loop_id: row.get("loop_id")?,
        run_id: row.get("run_id")?,
        vendor: row.get("vendor")?,
        status: row.get("status")?,
        pid: row.get("pid")?,
        command_json: row.get("command_json")?,
        cwd: row.get("cwd")?,
        summary: row.get("summary")?,
        parse_status: row.get("parse_status")?,
        completion_signal: row.get("completion_signal")?,
        heartbeat_count: row.get("heartbeat_count")?,
        last_heartbeat_at: row.get("last_heartbeat_at")?,
        output_json: row.get("output_json")?,
        error_message: row.get("error_message")?,
        native_session_id: row.get("native_session_id")?,
        native_resume_mode: row.get("native_resume_mode")?,
        native_resume_status: row.get("native_resume_status")?,
        native_resume_error: row.get("native_resume_error")?,
        started_at: row.get("started_at")?,
        ended_at: row.get("ended_at")?,
        metadata_json: row.get("metadata_json")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

const AE_COLUMNS: &str =
    "id, project_id, loop_id, run_id, vendor, status, pid, command_json, cwd, summary, parse_status, completion_signal, heartbeat_count, last_heartbeat_at, output_json, error_message, native_session_id, native_resume_mode, native_resume_status, native_resume_error, started_at, ended_at, metadata_json, created_at, updated_at";

#[derive(Clone)]
pub struct AgentExecutionsRepository {
    conn: Arc<Connection>,
}

impl AgentExecutionsRepository {
    pub fn new(conn: Arc<Connection>) -> Self {
        Self { conn }
    }

    pub fn upsert(&self, record: &AgentExecutionRecord) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO agent_executions (id, project_id, loop_id, run_id, vendor, status, pid, command_json, cwd, summary, parse_status, completion_signal, heartbeat_count, last_heartbeat_at, output_json, error_message, native_session_id, native_resume_mode, native_resume_status, native_resume_error, started_at, ended_at, metadata_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25)",
            rusqlite::params![
                &record.id,
                &record.project_id,
                &record.loop_id,
                &record.run_id,
                &record.vendor,
                &record.status,
                &record.pid,
                &record.command_json,
                &record.cwd,
                &record.summary,
                &record.parse_status,
                &record.completion_signal,
                &record.heartbeat_count,
                &record.last_heartbeat_at,
                &record.output_json,
                &record.error_message,
                &record.native_session_id,
                &record.native_resume_mode,
                &record.native_resume_status,
                &record.native_resume_error,
                &record.started_at,
                &record.ended_at,
                &record.metadata_json,
                &record.created_at,
                &record.updated_at,
            ],
        )?;
        Ok(())
    }

    /// Patch terminal fields without wiping identity columns (cwd, pid, metadata, …).
    ///
    /// Prefer this over `upsert` + partial defaults — `INSERT OR REPLACE` would
    /// null out project/loop/run/cwd and break orphan recovery / cleanup.
    pub fn update_terminal(
        &self,
        id: &str,
        status: &str,
        summary: Option<&str>,
        parse_status: Option<&str>,
        completion_signal: Option<&str>,
        error_message: Option<&str>,
        native_session_id: Option<&str>,
        heartbeat_count: i64,
        last_heartbeat_at: Option<&str>,
        ended_at: &str,
        updated_at: &str,
    ) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE agent_executions SET
                status = ?2,
                summary = ?3,
                parse_status = ?4,
                completion_signal = ?5,
                error_message = ?6,
                native_session_id = COALESCE(?7, native_session_id),
                heartbeat_count = ?8,
                last_heartbeat_at = ?9,
                ended_at = ?10,
                updated_at = ?11
             WHERE id = ?1",
            rusqlite::params![
                id,
                status,
                summary,
                parse_status,
                completion_signal,
                error_message,
                native_session_id,
                heartbeat_count,
                last_heartbeat_at,
                ended_at,
                updated_at,
            ],
        )?;
        if n == 0 {
            tracing::warn!("update_terminal: no agent_executions row for id={id}");
        }
        Ok(())
    }

    /// Patch only status (+ updated_at), e.g. `running` → `cancelling`.
    pub fn update_status(&self, id: &str, status: &str, updated_at: &str) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE agent_executions SET status = ?2, updated_at = ?3 WHERE id = ?1",
            rusqlite::params![id, status, updated_at],
        )?;
        if n == 0 {
            tracing::warn!("update_status: no agent_executions row for id={id}");
        }
        Ok(())
    }

    pub fn get_by_id(&self, id: &str) -> Result<Option<AgentExecutionRecord>> {
        let sql = format!("SELECT {} FROM agent_executions WHERE id = ?1", AE_COLUMNS);
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query_map(rusqlite::params![id], scan_agent_execution_row)?;
        match rows.next() {
            Some(Ok(record)) => Ok(Some(record)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    pub fn get_latest_by_run_id(&self, run_id: &str) -> Result<Option<AgentExecutionRecord>> {
        let sql =
            format!("SELECT {} FROM agent_executions WHERE run_id = ?1 ORDER BY started_at DESC LIMIT 1", AE_COLUMNS);
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query_map(rusqlite::params![run_id], scan_agent_execution_row)?;
        match rows.next() {
            Some(Ok(record)) => Ok(Some(record)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    pub fn get_latest_active_by_run_id(&self, run_id: &str) -> Result<Option<AgentExecutionRecord>> {
        let sql = format!(
            "SELECT {} FROM agent_executions WHERE run_id = ?1 AND status IN ('running', 'cancelling') ORDER BY started_at DESC LIMIT 1",
            AE_COLUMNS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query_map(rusqlite::params![run_id], scan_agent_execution_row)?;
        match rows.next() {
            Some(Ok(record)) => Ok(Some(record)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    pub fn get_latest_by_loop_id(&self, loop_id: &str) -> Result<Option<AgentExecutionRecord>> {
        let sql =
            format!("SELECT {} FROM agent_executions WHERE loop_id = ?1 ORDER BY started_at DESC LIMIT 1", AE_COLUMNS);
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query_map(rusqlite::params![loop_id], scan_agent_execution_row)?;
        match rows.next() {
            Some(Ok(record)) => Ok(Some(record)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    pub fn list_active(&self) -> Result<Vec<AgentExecutionRecord>> {
        let sql = format!(
            "SELECT {} FROM agent_executions WHERE status IN ('running', 'cancelling') ORDER BY started_at",
            AE_COLUMNS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], scan_agent_execution_row)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }

    pub fn list(&self) -> Result<Vec<AgentExecutionRecord>> {
        let sql = format!("SELECT {} FROM agent_executions", AE_COLUMNS);
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], scan_agent_execution_row)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }

    pub fn list_since(&self, since_iso: &str) -> Result<Vec<AgentExecutionRecord>> {
        let sql =
            format!("SELECT {} FROM agent_executions WHERE created_at >= ?1 ORDER BY created_at DESC", AE_COLUMNS);
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params![since_iso], scan_agent_execution_row)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }
}
