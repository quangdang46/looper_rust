use std::sync::Arc;

use rusqlite::Connection;
use crate::error::Result;
use crate::record::EventLogRecord;

fn scan_event_log_row(row: &rusqlite::Row) -> rusqlite::Result<EventLogRecord> {
    Ok(EventLogRecord {
        id: row.get("id")?,
        event_type: row.get("event_type")?,
        project_id: row.get("project_id")?,
        loop_id: row.get("loop_id")?,
        run_id: row.get("run_id")?,
        entity_type: row.get("entity_type")?,
        entity_id: row.get("entity_id")?,
        correlation_id: row.get("correlation_id")?,
        causation_id: row.get("causation_id")?,
        actor_type: row.get("actor_type")?,
        actor_id: row.get("actor_id")?,
        actor_display_name: row.get("actor_display_name")?,
        payload_json: row.get("payload_json")?,
        created_at: row.get("created_at")?,
    })
}

const EV_COLUMNS: &str =
    "id, event_type, project_id, loop_id, run_id, entity_type, entity_id, correlation_id, causation_id, actor_type, actor_id, actor_display_name, payload_json, created_at";

pub struct EventsRepository {
    conn: Arc<Connection>,
}

impl EventsRepository {
    pub fn new(conn: Arc<Connection>) -> Self {
        Self { conn }
    }

    pub fn append(&self, record: &EventLogRecord) -> Result<()> {
        self.conn.execute(
            "INSERT INTO event_logs (id, event_type, project_id, loop_id, run_id, entity_type, entity_id, correlation_id, causation_id, actor_type, actor_id, actor_display_name, payload_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            rusqlite::params![
                &record.id,
                &record.event_type,
                &record.project_id,
                &record.loop_id,
                &record.run_id,
                &record.entity_type,
                &record.entity_id,
                &record.correlation_id,
                &record.causation_id,
                &record.actor_type,
                &record.actor_id,
                &record.actor_display_name,
                &record.payload_json,
                &record.created_at,
            ],
        )?;
        Ok(())
    }

    pub fn list(&self, limit: i64) -> Result<Vec<EventLogRecord>> {
        let sql = format!(
            "SELECT {} FROM event_logs ORDER BY created_at DESC LIMIT ?1",
            EV_COLUMNS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params![limit], scan_event_log_row)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }

    pub fn list_since(&self, since_iso: &str) -> Result<Vec<EventLogRecord>> {
        let sql = format!(
            "SELECT {} FROM event_logs WHERE created_at >= ?1 ORDER BY created_at DESC",
            EV_COLUMNS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params![since_iso], scan_event_log_row)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }

    pub fn list_by_entity(&self, entity_type: &str, entity_id: &str) -> Result<Vec<EventLogRecord>> {
        let sql = format!(
            "SELECT {} FROM event_logs WHERE entity_type = ?1 AND entity_id = ?2 ORDER BY created_at ASC",
            EV_COLUMNS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows =
            stmt.query_map(rusqlite::params![entity_type, entity_id], scan_event_log_row)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }
}
