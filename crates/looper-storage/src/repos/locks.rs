use std::sync::Arc;

use rusqlite::Connection;
use crate::error::Result;
use crate::record::LockRecord;

fn scan_lock(row: &rusqlite::Row) -> rusqlite::Result<LockRecord> {
    Ok(LockRecord {
        key: row.get("key")?,
        owner: row.get("owner")?,
        reason: row.get("reason")?,
        expires_at: row.get("expires_at")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

const LOCK_COLUMNS: &str = "key, owner, reason, expires_at, created_at, updated_at";

#[derive(Clone)]
pub struct LocksRepository {
    conn: Arc<Connection>,
}

impl LocksRepository {
    pub fn new(conn: Arc<Connection>) -> Self {
        Self { conn }
    }

    pub fn acquire(&self, record: &LockRecord) -> Result<bool> {
        let rows = self.conn.execute(
            "INSERT INTO locks (key, owner, reason, expires_at, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(key) DO UPDATE SET owner=excluded.owner, reason=excluded.reason, expires_at=excluded.expires_at, updated_at=excluded.updated_at
             WHERE locks.expires_at <= ?4",
            rusqlite::params![
                &record.key,
                &record.owner,
                &record.reason,
                &record.expires_at,
                &record.created_at,
                &record.updated_at,
            ],
        )?;
        Ok(rows > 0)
    }

    pub fn release(&self, key: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM locks WHERE key = ?1",
            rusqlite::params![key],
        )?;
        Ok(())
    }

    pub fn get(&self, key: &str) -> Result<Option<LockRecord>> {
        let sql = format!("SELECT {} FROM locks WHERE key = ?1", LOCK_COLUMNS);
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query_map(rusqlite::params![key], scan_lock)?;
        match rows.next() {
            Some(Ok(record)) => Ok(Some(record)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    pub fn refresh(&self, record: &LockRecord) -> Result<bool> {
        let rows = self.conn.execute(
            "UPDATE locks SET owner=?2, reason=?3, expires_at=?4, updated_at=?5 WHERE key=?1 AND owner=?2",
            rusqlite::params![
                &record.key,
                &record.owner,
                &record.reason,
                &record.expires_at,
                &record.updated_at,
            ],
        )?;
        Ok(rows > 0)
    }

    pub fn list_expired(&self, now_iso: &str) -> Result<Vec<LockRecord>> {
        let sql = format!(
            "SELECT {} FROM locks WHERE expires_at <= ?1 ORDER BY expires_at",
            LOCK_COLUMNS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params![now_iso], scan_lock)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }
}
