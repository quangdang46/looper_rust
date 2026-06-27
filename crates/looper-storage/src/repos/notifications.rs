use std::sync::Arc;

use crate::error::Result;
use crate::record::NotificationRecord;
use rusqlite::Connection;

fn scan_notification(row: &rusqlite::Row) -> rusqlite::Result<NotificationRecord> {
    Ok(NotificationRecord {
        id: row.get("id")?,
        project_id: row.get("project_id")?,
        loop_id: row.get("loop_id")?,
        run_id: row.get("run_id")?,
        entity_type: row.get("entity_type")?,
        entity_id: row.get("entity_id")?,
        channel: row.get("channel")?,
        level: row.get("level")?,
        title: row.get("title")?,
        subtitle: row.get("subtitle")?,
        body: row.get("body")?,
        status: row.get("status")?,
        dedupe_key: row.get("dedupe_key")?,
        error_message: row.get("error_message")?,
        payload_json: row.get("payload_json")?,
        sent_at: row.get("sent_at")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

pub struct NotificationsRepository {
    conn: Arc<Connection>,
}

impl NotificationsRepository {
    pub fn new(conn: Arc<Connection>) -> Self {
        Self { conn }
    }

    pub fn upsert(&self, record: &NotificationRecord) -> Result<()> {
        self.conn.execute(
            "INSERT INTO notifications (id, project_id, loop_id, run_id, entity_type, entity_id, channel, level, title, subtitle, body, status, dedupe_key, error_message, payload_json, sent_at, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
             ON CONFLICT(id) DO UPDATE SET
               project_id=excluded.project_id, loop_id=excluded.loop_id, run_id=excluded.run_id,
               entity_type=excluded.entity_type, entity_id=excluded.entity_id, channel=excluded.channel,
               level=excluded.level, title=excluded.title, subtitle=excluded.subtitle, body=excluded.body,
               status=excluded.status, dedupe_key=excluded.dedupe_key, error_message=excluded.error_message,
               payload_json=excluded.payload_json, sent_at=excluded.sent_at, updated_at=excluded.updated_at",
            rusqlite::params![
                &record.id, &record.project_id, &record.loop_id, &record.run_id,
                &record.entity_type, &record.entity_id, &record.channel, &record.level,
                &record.title, &record.subtitle, &record.body, &record.status,
                &record.dedupe_key, &record.error_message, &record.payload_json, &record.sent_at,
                &record.created_at, &record.updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn get_by_id(&self, id: &str) -> Result<Option<NotificationRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, project_id, loop_id, run_id, entity_type, entity_id, channel, level, title, subtitle, body, status, dedupe_key, error_message, payload_json, sent_at, created_at, updated_at
             FROM notifications WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(rusqlite::params![id], scan_notification)?;
        match rows.next() {
            Some(Ok(record)) => Ok(Some(record)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    pub fn list(&self, limit: i64) -> Result<Vec<NotificationRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, project_id, loop_id, run_id, entity_type, entity_id, channel, level, title, subtitle, body, status, dedupe_key, error_message, payload_json, sent_at, created_at, updated_at
             FROM notifications ORDER BY created_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![limit], scan_notification)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }

    pub fn get_latest_by_dedupe(&self, channel: &str, dedupe_key: &str) -> Result<Option<NotificationRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, project_id, loop_id, run_id, entity_type, entity_id, channel, level, title, subtitle, body, status, dedupe_key, error_message, payload_json, sent_at, created_at, updated_at
             FROM notifications WHERE channel=?1 AND dedupe_key=?2 ORDER BY created_at DESC LIMIT 1",
        )?;
        let mut rows = stmt.query_map(rusqlite::params![channel, dedupe_key], scan_notification)?;
        match rows.next() {
            Some(Ok(record)) => Ok(Some(record)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }
}
