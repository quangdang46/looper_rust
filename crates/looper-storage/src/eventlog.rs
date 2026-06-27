use chrono::Utc;
use tracing::warn;
use uuid::Uuid;

use crate::error::{Result, StorageError};
use crate::record::{AppendInput, EventLogRecord};
use crate::repos::events::EventsRepository;

/// Format a chrono DateTime as JavaScript ISO string: "YYYY-MM-DDTHH:mm:ss.000Z"
pub fn format_javascript_iso_string() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%S.000Z").to_string()
}

/// Generate a random event ID with the given prefix.
/// Format: "{prefix}_{32 hex chars}" (16 random bytes via UUID v4).
pub fn new_event_id(prefix: &str) -> String {
    let p = if prefix.is_empty() { "event" } else { prefix };
    let hex_id = Uuid::new_v4().to_string().replace('-', "");
    format!("{}_{}", p, hex_id)
}

/// Append a structured event to the event log.
/// Handles ID generation, timestamp formatting, actor defaults, and payload resolution.
pub fn append(events: &EventsRepository, input: &AppendInput) -> Result<EventLogRecord> {
    let created_at = input.created_at.clone().unwrap_or_else(format_javascript_iso_string);

    let id = input.id.clone().unwrap_or_else(|| new_event_id("event"));

    let payload_json = if let Some(ref pj) = input.payload_json { pj.clone() } else { "{}".to_string() };

    // Actor defaults
    let actor_type = input.actor_type.clone().or_else(|| Some("system".to_string()));
    let actor_id = input.actor_id.clone().or_else(|| Some("looperd".to_string()));
    let actor_display_name = input.actor_display_name.clone().or_else(|| Some("looperd".to_string()));

    let record = EventLogRecord {
        id,
        event_type: input.event_type.clone(),
        project_id: input.project_id.clone(),
        loop_id: input.loop_id.clone(),
        run_id: input.run_id.clone(),
        entity_type: input.entity_type.clone(),
        entity_id: input.entity_id.clone(),
        correlation_id: input.correlation_id.clone(),
        causation_id: input.causation_id.clone(),
        actor_type,
        actor_id,
        actor_display_name,
        payload_json,
        created_at,
    };

    events.append(&record).map_err(|e| {
        warn!(error = %e, "Failed to append event log entry");
        StorageError::EventLog(format!("failed to append event: {e}"))
    })?;

    Ok(record)
}

/// EventLog service facade.
pub struct EventLog {
    events: EventsRepository,
}

impl EventLog {
    pub fn new(events: EventsRepository) -> Self {
        Self { events }
    }

    /// Append an event with the given type and optional configuration via `AppendInput`.
    pub fn emit(&self, input: &AppendInput) -> Result<EventLogRecord> {
        append(&self.events, input)
    }

    /// Convenience: create an AppendInput with just the event type.
    pub fn input(event_type: impl Into<String>) -> AppendInput {
        AppendInput::new(event_type)
    }
}
