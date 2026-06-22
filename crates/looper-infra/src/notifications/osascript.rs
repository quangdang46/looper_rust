//! macOS notification backend via `osascript`.

use std::process::Command;
use std::sync::Mutex;

use super::{Gateway, Notification};
use crate::error::NotifyError;

/// Sends macOS notifications via `osascript -e 'display notification ...'`.
pub struct OsascriptBackend {
    osascript_path: String,
    enabled: bool,
    /// Track the last sent time per key for throttling.
    last_sent: Mutex<std::collections::HashMap<String, std::time::Instant>>,
}

impl OsascriptBackend {
    pub fn new(osascript_path: Option<String>, enabled: bool) -> Self {
        Self {
            osascript_path: osascript_path.unwrap_or_else(|| "osascript".to_string()),
            enabled,
            last_sent: Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Send notification, respecting a minimum interval per key.
    pub fn send_throttled(&self, notification: &Notification, min_interval_secs: u64) -> Result<(), NotifyError> {
        if !self.enabled {
            return Ok(());
        }

        let throttle_key = format!("{}:{}", notification.event_type, notification.title);

        let mut last_sent = self.last_sent.lock().map_err(|e| {
            NotifyError::Throttle(format!("lock poisoned: {e}"))
        })?;

        let now = std::time::Instant::now();
        if let Some(last) = last_sent.get(&throttle_key) {
            if now.duration_since(*last).as_secs() < min_interval_secs {
                return Ok(()); // Skip, throttled
            }
        }
        last_sent.insert(throttle_key, now);
        drop(last_sent);

        self.send(notification)
    }
}

impl Gateway for OsascriptBackend {
    fn send(&self, notification: &Notification) -> Result<(), NotifyError> {
        if !self.enabled {
            return Ok(());
        }

        let title = &notification.title;
        let subtitle = notification.subtitle.as_deref().unwrap_or("");
        let body = &notification.body;

        let script = format!(
            r#"display notification "{}" with title "Looper" subtitle "{} – {}""#,
            body.replace('"', "\\\""),
            title.replace('"', "\\\""),
            subtitle.replace('"', "\\\""),
        );

        #[allow(clippy::disallowed_methods)]
        let output = Command::new(&self.osascript_path)
            .arg("-e")
            .arg(&script)
            .output()
            .map_err(|e| NotifyError::Osascript(e.to_string()))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(NotifyError::Osascript(stderr.to_string()));
        }

        Ok(())
    }

    fn throttle_key(&self) -> Option<&str> {
        Some("osascript")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notifications::NotificationLevel;

    #[test]
    fn test_throttle_blocks_duplicates() {
        let backend = OsascriptBackend::new(None, true);

        let n = Notification {
            event_type: "test.event".into(),
            project_id: None,
            loop_id: None,
            run_id: None,
            entity_type: None,
            entity_id: None,
            title: "Test".into(),
            subtitle: None,
            body: "body".into(),
            level: NotificationLevel::Info,
        };

        // First send should be accepted
        assert!(backend.send_throttled(&n, 60).is_ok());

        // Second within 60s should be throttled (but not error)
        assert!(backend.send_throttled(&n, 60).is_ok());
    }

    #[test]
    fn test_disabled_backend_returns_ok() {
        let backend = OsascriptBackend::new(None, false);
        let n = Notification::new("test", "title", "body");
        assert!(backend.send(&n).is_ok());
    }
}
