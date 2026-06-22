use std::sync::Arc;

use rusqlite::Connection;
use crate::error::Result;
use crate::helpers::bool_to_int;
use crate::record::WebhookTunnelHookRecord;

fn scan_webhook_tunnel_hook(row: &rusqlite::Row) -> rusqlite::Result<WebhookTunnelHookRecord> {
    Ok(WebhookTunnelHookRecord {
        repo: row.get("repo")?,
        hook_id: row.get("hook_id")?,
        managed_url: row.get("managed_url")?,
        secret_ref: row.get("secret_ref")?,
        last_ping_at: row.get("last_ping_at")?,
        consecutive_disables: row.get("consecutive_disables")?,
        last_disable_at: row.get("last_disable_at")?,
        orphaned: row.get::<_, i32>("orphaned")? != 0,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

const WHTH_COLUMNS: &str =
    "repo, hook_id, managed_url, secret_ref, last_ping_at, consecutive_disables, last_disable_at, orphaned, created_at, updated_at";

pub struct WebhookTunnelHooksRepository {
    conn: Arc<Connection>,
}

impl WebhookTunnelHooksRepository {
    pub fn new(conn: Arc<Connection>) -> Self {
        Self { conn }
    }

    pub fn list(&self) -> Result<Vec<WebhookTunnelHookRecord>> {
        let sql = format!("SELECT {} FROM webhook_tunnel_hooks ORDER BY repo", WHTH_COLUMNS);
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], scan_webhook_tunnel_hook)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }

    pub fn get(&self, repo: &str) -> Result<(Option<WebhookTunnelHookRecord>, bool)> {
        let sql = format!("SELECT {} FROM webhook_tunnel_hooks WHERE repo = ?1", WHTH_COLUMNS);
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query_map(rusqlite::params![repo], scan_webhook_tunnel_hook)?;
        match rows.next() {
            Some(Ok(record)) => Ok((Some(record), true)),
            Some(Err(e)) => Err(e.into()),
            None => Ok((None, false)),
        }
    }

    pub fn upsert(&self, record: &WebhookTunnelHookRecord) -> Result<()> {
        self.conn.execute(
            "INSERT INTO webhook_tunnel_hooks (repo, hook_id, managed_url, secret_ref, last_ping_at, consecutive_disables, last_disable_at, orphaned, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(repo) DO UPDATE SET
               hook_id=excluded.hook_id, managed_url=excluded.managed_url, secret_ref=excluded.secret_ref,
               last_ping_at=excluded.last_ping_at, consecutive_disables=excluded.consecutive_disables,
               last_disable_at=excluded.last_disable_at, orphaned=excluded.orphaned,
               updated_at=excluded.updated_at",
            rusqlite::params![
                &record.repo, &record.hook_id, &record.managed_url, &record.secret_ref,
                &record.last_ping_at, &record.consecutive_disables, &record.last_disable_at,
                bool_to_int(record.orphaned), &record.created_at, &record.updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn mark_orphaned(&self, repo: &str, orphaned: bool, updated_at: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE webhook_tunnel_hooks SET orphaned=?2, updated_at=?3 WHERE repo=?1",
            rusqlite::params![repo, bool_to_int(orphaned), updated_at],
        )?;
        Ok(())
    }

    pub fn delete(&self, repo: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM webhook_tunnel_hooks WHERE repo = ?1",
            rusqlite::params![repo],
        )?;
        Ok(())
    }

    pub fn update_ping(&self, repo: &str, at: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE webhook_tunnel_hooks SET last_ping_at=?2 WHERE repo=?1",
            rusqlite::params![repo, at],
        )?;
        Ok(())
    }
}
