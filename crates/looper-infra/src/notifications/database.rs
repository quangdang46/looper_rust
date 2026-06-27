//! Database notification backend — persists notifications via the event log.

use std::sync::Mutex;

use looper_storage::eventlog::EventLog;
use looper_storage::record::AppendInput;

use super::{Gateway, Notification};
use crate::error::NotifyError;

/// Writes notifications as `notification.*` events into the event log.
///
/// Uses `Mutex<()>` for synchronization because `rusqlite::Connection` is not `Sync`
/// (it uses `RefCell` for interior mutability). The `unsafe` impls are justified by
/// the `Mutex` providing mutual exclusion.
pub struct DatabaseBackend {
    event_log: EventLog,
    lock: Mutex<()>,
}

// SAFETY: DatabaseBackend serializes all access via `lock`, making it safe to
// share across threads despite `rusqlite::Connection` being `!Sync`.
unsafe impl Send for DatabaseBackend {}
unsafe impl Sync for DatabaseBackend {}

impl DatabaseBackend {
    pub fn new(event_log: EventLog) -> Self {
        Self { event_log, lock: Mutex::new(()) }
    }
}

impl Gateway for DatabaseBackend {
    fn send(&self, notification: &Notification) -> Result<(), NotifyError> {
        let _guard = self.lock.lock().map_err(|e| NotifyError::EventLog(format!("lock: {e}")))?;

        let payload = serde_json::json!({
            "title": notification.title,
            "subtitle": notification.subtitle,
            "body": notification.body,
            "level": notification.level.to_string(),
        });

        let input = AppendInput {
            event_type: format!("notification.{}", notification.event_type),
            project_id: notification.project_id.clone(),
            loop_id: notification.loop_id.clone(),
            run_id: notification.run_id.clone(),
            entity_type: notification.entity_type.clone(),
            entity_id: notification.entity_id.clone(),
            payload_json: Some(serde_json::to_string(&payload).unwrap_or_default()),
            ..AppendInput::new("")
        };

        self.event_log.emit(&input).map_err(|e| NotifyError::EventLog(e.to_string()))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use looper_storage::eventlog::new_event_id;

    #[test]
    fn test_new_event_id_format() {
        let id = new_event_id("test");
        assert!(id.starts_with("test_"));
        assert_eq!(id.len(), 4 + 1 + 32);
    }
}
