use std::sync::Arc;

use crate::error::Result;
use crate::record::TakeoverSessionRecord;
use rusqlite::Connection;

fn scan_takeover_row(row: &rusqlite::Row) -> rusqlite::Result<TakeoverSessionRecord> {
    Ok(TakeoverSessionRecord {
        id: row.get("id")?,
        project_name: row.get("project_name")?,
        repo_owner: row.get("repo_owner")?,
        repo_name: row.get("repo_name")?,
        pr_number: row.get("pr_number")?,
        status: row.get("status")?,
        started_at: row.get("started_at")?,
        last_activity: row.get("last_activity")?,
        cycles_completed: row.get("cycles_completed")?,
        current_phase: row.get("current_phase")?,
        error_count: row.get("error_count")?,
        max_errors: row.get("max_errors")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

const TAKEOVER_COLUMNS: &str = "id, project_name, repo_owner, repo_name, pr_number, status, started_at, last_activity, cycles_completed, current_phase, error_count, max_errors, created_at, updated_at";

pub struct TakeoverSessionsRepository {
    conn: Arc<Connection>,
}

impl TakeoverSessionsRepository {
    pub fn new(conn: Arc<Connection>) -> Self {
        Self { conn }
    }

    pub fn upsert(&self, record: &TakeoverSessionRecord) -> Result<()> {
        let updated = self.conn.execute(
            "UPDATE takeover_sessions SET project_name=?2, repo_owner=?3, repo_name=?4, pr_number=?5, status=?6, started_at=?7, last_activity=?8, cycles_completed=?9, current_phase=?10, error_count=?11, max_errors=?12, updated_at=?14 WHERE id=?1",
            rusqlite::params![
                &record.id,
                &record.project_name,
                &record.repo_owner,
                &record.repo_name,
                &record.pr_number,
                &record.status,
                &record.started_at,
                &record.last_activity,
                &record.cycles_completed,
                &record.current_phase,
                &record.error_count,
                &record.max_errors,
                &record.created_at,
                &record.updated_at,
            ],
        )?;
        if updated == 0 {
            self.conn.execute(
                "INSERT INTO takeover_sessions (id, project_name, repo_owner, repo_name, pr_number, status, started_at, last_activity, cycles_completed, current_phase, error_count, max_errors, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                rusqlite::params![
                    &record.id,
                    &record.project_name,
                    &record.repo_owner,
                    &record.repo_name,
                    &record.pr_number,
                    &record.status,
                    &record.started_at,
                    &record.last_activity,
                    &record.cycles_completed,
                    &record.current_phase,
                    &record.error_count,
                    &record.max_errors,
                    &record.created_at,
                    &record.updated_at,
                ],
            )?;
        }
        Ok(())
    }

    pub fn get_by_id(&self, id: &str) -> Result<Option<TakeoverSessionRecord>> {
        let sql = format!("SELECT {TAKEOVER_COLUMNS} FROM takeover_sessions WHERE id = ?1");
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query_map(rusqlite::params![id], scan_takeover_row)?;
        match rows.next() {
            Some(Ok(record)) => Ok(Some(record)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    pub fn list_active(&self) -> Result<Vec<TakeoverSessionRecord>> {
        let sql = format!(
            "SELECT {TAKEOVER_COLUMNS} FROM takeover_sessions WHERE status = 'active' ORDER BY started_at DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], scan_takeover_row)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }

    pub fn list_all(&self) -> Result<Vec<TakeoverSessionRecord>> {
        let sql = format!("SELECT {TAKEOVER_COLUMNS} FROM takeover_sessions ORDER BY started_at DESC");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], scan_takeover_row)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }

    pub fn update_status(&self, id: &str, status: &str, updated_at: &str) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE takeover_sessions SET status=?2, updated_at=?3 WHERE id=?1",
            rusqlite::params![id, status, updated_at],
        )?;
        if n == 0 {
            return Err(crate::error::StorageError::NotFound(format!("takeover session {id}")));
        }
        Ok(())
    }

    pub fn update_progress(
        &self,
        id: &str,
        cycles_completed: i32,
        current_phase: &str,
        error_count: i32,
        last_activity: &str,
        updated_at: &str,
    ) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE takeover_sessions SET cycles_completed=?2, current_phase=?3, error_count=?4, last_activity=?5, updated_at=?6 WHERE id=?1",
            rusqlite::params![id, cycles_completed, current_phase, error_count, last_activity, updated_at],
        )?;
        if n == 0 {
            return Err(crate::error::StorageError::NotFound(format!("takeover session {id}")));
        }
        Ok(())
    }

    pub fn delete_by_project(&self, project_name: &str) -> Result<i64> {
        let n = self
            .conn
            .execute("DELETE FROM takeover_sessions WHERE project_name = ?1", rusqlite::params![project_name])?;
        Ok(n as i64)
    }
}
