//! Notification subsystem — abstract `Gateway` trait, database/osascript backends, throttle.

pub mod database;
pub mod osascript;
pub mod throttle;

use std::sync::Arc;

use crate::error::NotifyError;

/// A notification event.
#[derive(Debug, Clone)]
pub struct Notification {
    pub event_type: String,
    pub project_id: Option<String>,
    pub loop_id: Option<String>,
    pub run_id: Option<String>,
    pub entity_type: Option<String>,
    pub entity_id: Option<String>,
    pub title: String,
    pub subtitle: Option<String>,
    pub body: String,
    pub level: NotificationLevel,
}

impl Notification {
    pub fn new(event_type: impl Into<String>, title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            event_type: event_type.into(),
            project_id: None,
            loop_id: None,
            run_id: None,
            entity_type: None,
            entity_id: None,
            title: title.into(),
            subtitle: None,
            body: body.into(),
            level: NotificationLevel::Info,
        }
    }

    pub fn with_level(mut self, level: NotificationLevel) -> Self {
        self.level = level;
        self
    }
}

/// Notification severity level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationLevel {
    Debug,
    Info,
    Warn,
    Error,
}

impl std::fmt::Display for NotificationLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Debug => write!(f, "debug"),
            Self::Info => write!(f, "info"),
            Self::Warn => write!(f, "warn"),
            Self::Error => write!(f, "error"),
        }
    }
}

/// Abstract notification gateway — implemented by each backend.
pub trait Gateway: Send + Sync {
    fn send(&self, notification: &Notification) -> Result<(), NotifyError>;
    fn throttle_key(&self) -> Option<&str> {
        None
    }
}

/// Composite gateway that fans out to multiple backends.
pub struct CompositeGateway {
    backends: Vec<(String, Arc<dyn Gateway>)>,
    throttle: Arc<throttle::Throttle>,
}

impl CompositeGateway {
    pub fn new(throttle: Arc<throttle::Throttle>) -> Self {
        Self { backends: Vec::new(), throttle }
    }

    pub fn register(&mut self, name: impl Into<String>, gateway: Arc<dyn Gateway>) {
        self.backends.push((name.into(), gateway));
    }

    pub fn send(&self, notification: &Notification) -> Result<(), NotifyError> {
        for (name, backend) in &self.backends {
            // Throttle check
            if let Some(key) = backend.throttle_key() {
                let throttle_key = format!("{}_{}", key, notification.event_type);
                if !self.throttle.should_send(&throttle_key) {
                    continue;
                }
            }
            if let Err(e) = backend.send(notification) {
                tracing::warn!(backend = %name, error = %e, "notification send failed");
            }
        }
        Ok(())
    }

    pub fn backends(&self) -> &[(String, Arc<dyn Gateway>)] {
        &self.backends
    }
}
