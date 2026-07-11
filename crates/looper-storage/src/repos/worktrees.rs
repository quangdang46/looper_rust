use std::sync::Arc;

use crate::error::Result;
use crate::record::WorktreeRecord;
use rusqlite::Connection;

fn scan_worktree(row: &rusqlite::Row) -> rusqlite::Result<WorktreeRecord> {
    Ok(WorktreeRecord {
        id: row.get("id")?,
        project_id: row.get("project_id")?,
        repo_path: row.get("repo_path")?,
        worktree_path: row.get("worktree_path")?,
        branch: row.get("branch")?,
        base_branch: row.get("base_branch")?,
        status: row.get("status")?,
        head_sha: row.get("head_sha")?,
        metadata_json: row.get("metadata_json")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
        cleaned_at: row.get("cleaned_at")?,
    })
}

const WT_COLUMNS: &str =
    "id, project_id, repo_path, worktree_path, branch, base_branch, status, head_sha, metadata_json, created_at, updated_at, cleaned_at";

pub struct WorktreesRepository {
    conn: Arc<Connection>,
}

impl WorktreesRepository {
    pub fn new(conn: Arc<Connection>) -> Self {
        Self { conn }
    }

    pub fn upsert(&self, record: &WorktreeRecord) -> Result<()> {
        self.conn.execute(
            "INSERT INTO worktrees (id, project_id, repo_path, worktree_path, branch, base_branch, status, head_sha, metadata_json, created_at, updated_at, cleaned_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(id) DO UPDATE SET
               project_id=excluded.project_id, repo_path=excluded.repo_path, worktree_path=excluded.worktree_path,
               branch=excluded.branch, base_branch=excluded.base_branch, status=excluded.status,
               head_sha=excluded.head_sha, metadata_json=excluded.metadata_json, updated_at=excluded.updated_at,
               cleaned_at=excluded.cleaned_at",
            rusqlite::params![
                &record.id, &record.project_id, &record.repo_path, &record.worktree_path,
                &record.branch, &record.base_branch, &record.status, &record.head_sha,
                &record.metadata_json, &record.created_at, &record.updated_at, &record.cleaned_at,
            ],
        )?;
        Ok(())
    }

    pub fn get_by_id(&self, id: &str) -> Result<Option<WorktreeRecord>> {
        let sql = format!("SELECT {} FROM worktrees WHERE id = ?1", WT_COLUMNS);
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query_map(rusqlite::params![id], scan_worktree)?;
        match rows.next() {
            Some(Ok(record)) => Ok(Some(record)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    pub fn get_by_branch(&self, project_id: &str, branch: &str) -> Result<Option<WorktreeRecord>> {
        let sql = format!("SELECT {} FROM worktrees WHERE project_id = ?1 AND branch = ?2", WT_COLUMNS);
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query_map(rusqlite::params![project_id, branch], scan_worktree)?;
        match rows.next() {
            Some(Ok(record)) => Ok(Some(record)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    pub fn list_by_project(&self, project_id: &str) -> Result<Vec<WorktreeRecord>> {
        let sql = format!("SELECT {} FROM worktrees WHERE project_id = ?1 ORDER BY created_at DESC", WT_COLUMNS);
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params![project_id], scan_worktree)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }

    pub fn list_cleanup_candidates(&self, limit: i32) -> Result<Vec<WorktreeRecord>> {
        let sql = format!(
            "SELECT {} FROM worktrees WHERE cleaned_at IS NULL AND status != 'active' ORDER BY updated_at ASC LIMIT ?1",
            WT_COLUMNS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params![limit], scan_worktree)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }

    pub fn list_active(&self) -> Result<Vec<WorktreeRecord>> {
        let sql = format!("SELECT {} FROM worktrees WHERE status = 'active' ORDER BY updated_at DESC", WT_COLUMNS);
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], scan_worktree)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }

    /// Latest non-cleaned worktree for a loop id.
    ///
    /// Matches planner/reviewer conventions:
    /// - `metadata_json.loop_id`
    /// - branch ending with `/{loop_id}` (e.g. `planner/{id}`, `review/{id}`)
    pub fn get_latest_by_loop_id(&self, loop_id: &str) -> Result<Option<WorktreeRecord>> {
        let sql = format!(
            "SELECT {} FROM worktrees
             WHERE cleaned_at IS NULL
               AND (
                 json_extract(COALESCE(metadata_json, '{{}}'), '$.loop_id') = ?1
                 OR branch LIKE ?2
               )
             ORDER BY
               CASE WHEN status = 'active' THEN 0 ELSE 1 END,
               updated_at DESC
             LIMIT 1",
            WT_COLUMNS
        );
        let branch_suffix = format!("%/{}", loop_id);
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query_map(rusqlite::params![loop_id, branch_suffix], scan_worktree)?;
        match rows.next() {
            Some(Ok(record)) => Ok(Some(record)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    pub fn touch_cleanup_attempt(&self, id: &str, updated_at: &str) -> Result<()> {
        self.conn.execute("UPDATE worktrees SET updated_at = ?2 WHERE id = ?1", rusqlite::params![id, updated_at])?;
        Ok(())
    }
}
