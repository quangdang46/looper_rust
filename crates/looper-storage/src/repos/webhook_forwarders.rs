use std::sync::Arc;

use rusqlite::Connection;
use crate::error::Result;
use crate::record::WebhookForwarderRecord;

fn scan_webhook_forwarder(row: &rusqlite::Row) -> rusqlite::Result<WebhookForwarderRecord> {
    Ok(WebhookForwarderRecord {
        repo: row.get("repo")?,
        pid: row.get("pid")?,
        process_start: row.get("process_start")?,
        fingerprint: row.get("fingerprint")?,
        endpoint: row.get("endpoint")?,
        events: row.get("events")?,
        gh_path: row.get("gh_path")?,
        daemon_id: row.get("daemon_id")?,
        spawned_at: row.get("spawned_at")?,
        updated_at: row.get("updated_at")?,
    })
}

const WHF_COLUMNS: &str =
    "repo, pid, process_start, fingerprint, endpoint, events, gh_path, daemon_id, spawned_at, updated_at";

pub struct WebhookForwardersRepository {
    conn: Arc<Connection>,
}

impl WebhookForwardersRepository {
    pub fn new(conn: Arc<Connection>) -> Self {
        Self { conn }
    }

    pub fn list(&self) -> Result<Vec<WebhookForwarderRecord>> {
        let sql = format!("SELECT {} FROM webhook_forwarders ORDER BY repo", WHF_COLUMNS);
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], scan_webhook_forwarder)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }

    pub fn upsert(&self, record: &WebhookForwarderRecord) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO webhook_forwarders (repo, pid, process_start, fingerprint, endpoint, events, gh_path, daemon_id, spawned_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                &record.repo, &record.pid, &record.process_start, &record.fingerprint,
                &record.endpoint, &record.events, &record.gh_path, &record.daemon_id,
                &record.spawned_at, &record.updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn delete(&self, repo: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM webhook_forwarders WHERE repo = ?1",
            rusqlite::params![repo],
        )?;
        Ok(())
    }
}
