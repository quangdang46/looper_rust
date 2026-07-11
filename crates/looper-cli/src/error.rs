use std::fmt;

#[derive(Debug)]
pub enum CliError {
    Http(reqwest::Error),
    Json(serde_json::Error),
    Io(std::io::Error),
    Api { code: String, message: String },
    DaemonNotRunning,
    Config(String),
    Autoupgrade(String),
    DaemonLifecycle(String),
    Other(String),
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http(e) => write!(f, "HTTP error: {e}"),
            Self::Json(e) => write!(f, "JSON error: {e}"),
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::Api { code, message } => write!(f, "API error [{code}]: {message}"),
            Self::DaemonNotRunning => write!(f, "daemon is not running"),
            Self::Config(m) => write!(f, "config error: {m}"),
            Self::Autoupgrade(m) => write!(f, "autoupgrade error: {m}"),
            Self::DaemonLifecycle(m) => write!(f, "daemon lifecycle error: {m}"),
            Self::Other(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for CliError {}

impl From<reqwest::Error> for CliError {
    fn from(e: reqwest::Error) -> Self {
        Self::Http(e)
    }
}

impl From<serde_json::Error> for CliError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

impl From<std::io::Error> for CliError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl CliError {
    pub fn api<T: Into<String>>(code: T, message: T) -> Self {
        Self::Api { code: code.into(), message: message.into() }
    }
    pub fn daemon_not_running() -> Self {
        Self::DaemonNotRunning
    }
    pub fn config<T: Into<String>>(m: T) -> Self {
        Self::Config(m.into())
    }
    pub fn autoupgrade<T: Into<String>>(m: T) -> Self {
        Self::Autoupgrade(m.into())
    }
    pub fn daemon_lifecycle<T: Into<String>>(m: T) -> Self {
        Self::DaemonLifecycle(m.into())
    }

    /// Stable error for disabled stub commands (exit non-zero; never fake success).
    pub fn unsupported(cmd: &str) -> Self {
        Self::Other(format!("unsupported: '{cmd}' was a stub and has been disabled; see docs"))
    }
}
