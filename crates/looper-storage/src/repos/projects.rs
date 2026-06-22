use std::sync::Arc;

use rusqlite::Connection;
use crate::error::Result;
use crate::record::ProjectRecord;
use crate::helpers::bool_to_int;

fn scan_project_row(row: &rusqlite::Row) -> rusqlite::Result<ProjectRecord> {
    Ok(ProjectRecord {
        id: row.get("id")?,
        name: row.get("name")?,
        repo_path: row.get("repo_path")?,
        base_branch: row.get("base_branch")?,
        archived: row.get::<_, i32>("archived")? != 0,
        metadata_json: row.get("metadata_json")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

pub struct ProjectsRepository {
    conn: Arc<Connection>,
}

impl ProjectsRepository {
    pub fn new(conn: Arc<Connection>) -> Self {
        Self { conn }
    }

    pub fn upsert(&self, record: &ProjectRecord) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO projects (id, name, repo_path, base_branch, archived, metadata_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                &record.id,
                &record.name,
                &record.repo_path,
                &record.base_branch,
                bool_to_int(record.archived),
                &record.metadata_json,
                &record.created_at,
                &record.updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn get_by_id(&self, id: &str) -> Result<Option<ProjectRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, repo_path, base_branch, archived, metadata_json, created_at, updated_at
             FROM projects WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(rusqlite::params![id], scan_project_row)?;
        match rows.next() {
            Some(Ok(record)) => Ok(Some(record)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    pub fn list(&self) -> Result<Vec<ProjectRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, repo_path, base_branch, archived, metadata_json, created_at, updated_at
             FROM projects ORDER BY name",
        )?;
        let rows = stmt.query_map([], scan_project_row)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }

    pub fn archive(&self, id: &str, updated_at: &str) -> Result<bool> {
        let mut stmt = self.conn.prepare(
            "UPDATE projects SET archived = 1, updated_at = ?2 WHERE id = ?1 RETURNING id",
        )?;
        let mut rows = stmt.query_map(rusqlite::params![id, updated_at], |row| {
            row.get::<_, String>("id")
        })?;
        match rows.next() {
            Some(Ok(_)) => Ok(true),
            Some(Err(e)) => Err(e.into()),
            None => Ok(false),
        }
    }
}
