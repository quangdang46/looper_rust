use std::sync::Arc;

use rusqlite::Connection;
use crate::error::Result;
use crate::record::PullRequestSnapshotRecord;

fn scan_pr_snapshot_row(row: &rusqlite::Row) -> rusqlite::Result<PullRequestSnapshotRecord> {
    Ok(PullRequestSnapshotRecord {
        id: row.get("id")?,
        project_id: row.get("project_id")?,
        repo: row.get("repo")?,
        pr_number: row.get("pr_number")?,
        head_sha: row.get("head_sha")?,
        base_sha: row.get("base_sha")?,
        title: row.get("title")?,
        body: row.get("body")?,
        author: row.get("author")?,
        diff_ref: row.get("diff_ref")?,
        checks_summary: row.get("checks_summary")?,
        unresolved_thread_count: row.get("unresolved_thread_count")?,
        review_state: row.get("review_state")?,
        payload_json: row.get("payload_json")?,
        captured_at: row.get("captured_at")?,
        created_at: row.get("created_at")?,
    })
}

const PRS_COLUMNS: &str =
    "id, project_id, repo, pr_number, head_sha, base_sha, title, body, author, diff_ref, checks_summary, unresolved_thread_count, review_state, payload_json, captured_at, created_at";

#[derive(Clone)]
pub struct PullRequestSnapshotsRepository {
    conn: Arc<Connection>,
}

impl PullRequestSnapshotsRepository {
    pub fn new(conn: Arc<Connection>) -> Self {
        Self { conn }
    }

    pub fn upsert(&self, record: &PullRequestSnapshotRecord) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO pull_request_snapshots (id, project_id, repo, pr_number, head_sha, base_sha, title, body, author, diff_ref, checks_summary, unresolved_thread_count, review_state, payload_json, captured_at, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            rusqlite::params![
                &record.id,
                &record.project_id,
                &record.repo,
                &record.pr_number,
                &record.head_sha,
                &record.base_sha,
                &record.title,
                &record.body,
                &record.author,
                &record.diff_ref,
                &record.checks_summary,
                &record.unresolved_thread_count,
                &record.review_state,
                &record.payload_json,
                &record.captured_at,
                &record.created_at,
            ],
        )?;
        Ok(())
    }

    pub fn list(&self) -> Result<Vec<PullRequestSnapshotRecord>> {
        let sql = format!("SELECT {} FROM pull_request_snapshots", PRS_COLUMNS);
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], scan_pr_snapshot_row)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }

    pub fn get_latest(&self, repo: &str, pr_number: i64) -> Result<Option<PullRequestSnapshotRecord>> {
        let sql = format!(
            "SELECT {} FROM pull_request_snapshots WHERE repo = ?1 AND pr_number = ?2 ORDER BY captured_at DESC LIMIT 1",
            PRS_COLUMNS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query_map(rusqlite::params![repo, pr_number], scan_pr_snapshot_row)?;
        match rows.next() {
            Some(Ok(record)) => Ok(Some(record)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    pub fn get_latest_by_project(
        &self,
        project_id: &str,
        repo: &str,
        pr_number: i64,
    ) -> Result<Option<PullRequestSnapshotRecord>> {
        let sql = format!(
            "SELECT {} FROM pull_request_snapshots WHERE project_id = ?1 AND repo = ?2 AND pr_number = ?3 ORDER BY captured_at DESC LIMIT 1",
            PRS_COLUMNS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows =
            stmt.query_map(rusqlite::params![project_id, repo, pr_number], scan_pr_snapshot_row)?;
        match rows.next() {
            Some(Ok(record)) => Ok(Some(record)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }
}
