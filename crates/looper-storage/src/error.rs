use thiserror::Error;

#[derive(Error, Debug)]
pub enum StorageError {
    #[error("rusqlite error: {0}")]
    Rusqlite(#[from] rusqlite::Error),

    #[error("serde_json error: {0}")]
    SerdeJson(#[from] serde_json::Error),

    #[error("chrono error: {0}")]
    Chrono(#[from] chrono::ParseError),

    #[error("migration error: {0}")]
    Migration(String),

    #[error("queue item not active: {0}")]
    QueueNotActive(String),

    #[error("entity not found: {0}")]
    NotFound(String),

    #[error("event log service: {0}")]
    EventLog(String),

    #[error("lock acquisition conflict: {0}")]
    LockConflict(String),

    #[error("repository not configured: {0}")]
    NotConfigured(String),
}

pub type Result<T> = std::result::Result<T, StorageError>;
