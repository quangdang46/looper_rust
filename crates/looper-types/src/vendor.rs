use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

use crate::error::DomainError;

/// Supported agent vendors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentVendor {
    Claude,
    OpenAi,
    Gemini,
    Grok,
    DeepSeek,
    Custom,
}

impl AgentVendor {
    pub fn as_str(self) -> &'static str {
        use AgentVendor::*;
        match self {
            Claude => "claude",
            OpenAi => "open_ai",
            Gemini => "gemini",
            Grok => "grok",
            DeepSeek => "deep_seek",
            Custom => "custom",
        }
    }
}

impl fmt::Display for AgentVendor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AgentVendor {
    type Err = DomainError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        use AgentVendor::*;
        match s {
            "claude" => Ok(Claude),
            "open_ai" => Ok(OpenAi),
            "gemini" => Ok(Gemini),
            "grok" => Ok(Grok),
            "deep_seek" => Ok(DeepSeek),
            "custom" => Ok(Custom),
            _ => Err(DomainError::UnknownAgentVendor { value: s.to_string() }),
        }
    }
}

/// Log level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub fn as_str(self) -> &'static str {
        use LogLevel::*;
        match self {
            Trace => "trace",
            Debug => "debug",
            Info => "info",
            Warn => "warn",
            Error => "error",
        }
    }
}

impl fmt::Display for LogLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for LogLevel {
    type Err = DomainError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        use LogLevel::*;
        match s {
            "trace" => Ok(Trace),
            "debug" => Ok(Debug),
            "info" => Ok(Info),
            "warn" => Ok(Warn),
            "error" => Ok(Error),
            _ => Err(DomainError::UnknownLogLevel { value: s.to_string() }),
        }
    }
}

/// Authentication modes for the REST API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    None,
    Token,
    Oidc,
}

impl AuthMode {
    pub fn as_str(self) -> &'static str {
        use AuthMode::*;
        match self {
            None => "none",
            Token => "token",
            Oidc => "oidc",
        }
    }
}

impl fmt::Display for AuthMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AuthMode {
    type Err = DomainError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        use AuthMode::*;
        match s {
            "none" => Ok(None),
            "token" => Ok(Token),
            "oidc" => Ok(Oidc),
            _ => Err(DomainError::UnknownAuthMode { value: s.to_string() }),
        }
    }
}

/// Daemon deployment mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonMode {
    Local,
    Cloud,
}

impl DaemonMode {
    pub fn as_str(self) -> &'static str {
        use DaemonMode::*;
        match self {
            Local => "local",
            Cloud => "cloud",
        }
    }
}

impl fmt::Display for DaemonMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for DaemonMode {
    type Err = DomainError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        use DaemonMode::*;
        match s {
            "local" => Ok(Local),
            "cloud" => Ok(Cloud),
            _ => Err(DomainError::UnknownDaemonMode { value: s.to_string() }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_vendor_roundtrip() {
        for v in [AgentVendor::Claude, AgentVendor::Gemini, AgentVendor::Custom] {
            let json = serde_json::to_string(&v).unwrap();
            let back: AgentVendor = serde_json::from_str(&json).unwrap();
            assert_eq!(v, back);
        }
    }

    #[test]
    fn test_agent_vendor_from_str() {
        assert_eq!("claude".parse(), Ok(AgentVendor::Claude));
        assert_eq!("open_ai".parse(), Ok(AgentVendor::OpenAi));
        assert!("unknown".parse::<AgentVendor>().is_err());
    }

    #[test]
    fn test_log_level_ordering() {
        assert_eq!(LogLevel::Debug.as_str(), "debug");
        assert_eq!(LogLevel::Info.to_string(), "info");
    }

    #[test]
    fn test_auth_mode() {
        assert_eq!(AuthMode::Oidc.to_string(), "oidc");
        assert_eq!("token".parse::<AuthMode>(), Ok(AuthMode::Token));
    }

    #[test]
    fn test_daemon_mode() {
        assert_eq!(DaemonMode::Cloud.as_str(), "cloud");
        assert_eq!("local".parse::<DaemonMode>(), Ok(DaemonMode::Local));
    }
}
