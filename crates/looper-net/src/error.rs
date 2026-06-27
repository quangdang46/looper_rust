/// Errors originating from the loopernet network layer.
#[derive(Debug, thiserror::Error)]
pub enum NetworkError {
    #[error("API error: {status} - {message}")]
    Api { status: u16, message: String },

    #[error("unauthorized: {0}")]
    Unauthorized(String),

    #[error("stale coordinator lease token")]
    StaleLeaseToken,

    #[error("protocol version mismatch: expected {expected}, got {got}")]
    ProtocolVersion { expected: String, got: String },

    #[error("daemon version {got} is below minimum {minimum}")]
    VersionTooLow { minimum: String, got: String },

    #[error("join key already consumed")]
    JoinKeyConsumed,

    #[error("node name already active: {0}")]
    NodeNameTaken(String),

    #[error("invalid node name: {0}")]
    InvalidNodeName(String),

    #[error("not joined to any network")]
    NotJoined,

    #[error("no active network node")]
    NoActiveNode,

    #[error("invalid join key")]
    InvalidJoinKey,

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("config error: {0}")]
    Config(#[from] looper_config::ConfigError),

    #[error("database error: {0}")]
    Database(String),

    #[error("{0}")]
    Other(String),
}

impl NetworkError {
    pub fn is_transient(&self) -> bool {
        matches!(self, NetworkError::Api { status: 500..=599, .. } | NetworkError::Http(_) | NetworkError::Io(_))
    }
}

impl From<rusqlite::Error> for NetworkError {
    fn from(e: rusqlite::Error) -> Self {
        NetworkError::Database(e.to_string())
    }
}
