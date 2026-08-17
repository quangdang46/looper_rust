//! Takeover session types — single-PR focus mode.
//!
//! A takeover session drives a specific PR through review → fix → merge cycles
//! until the PR is merged, failed, or cancelled by the operator.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Persistent takeover session tracked in the `takeover_sessions` table.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TakeoverSession {
    pub id: String,
    pub project_name: String,
    pub repo_owner: String,
    pub repo_name: String,
    pub pr_number: u64,
    pub status: TakeoverStatus,
    pub started_at: String,
    pub last_activity: String,
    pub cycles_completed: u32,
    pub current_phase: TakeoverPhase,
    pub error_count: u32,
    pub max_errors: u32,
    pub created_at: String,
    pub updated_at: String,
}

/// Lifecycle status of a takeover session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TakeoverStatus {
    Active,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

impl TakeoverStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

impl fmt::Display for TakeoverStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for TakeoverStatus {
    type Err = crate::error::DomainError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "active" => Ok(Self::Active),
            "paused" => Ok(Self::Paused),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(crate::error::DomainError::Other(format!("unknown takeover status: {s}"))),
        }
    }
}

/// Current phase within an active takeover cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TakeoverPhase {
    Planning,
    Reviewing,
    Fixing,
    Working,
    Merging,
    Waiting,
}

impl TakeoverPhase {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Planning => "planning",
            Self::Reviewing => "reviewing",
            Self::Fixing => "fixing",
            Self::Working => "working",
            Self::Merging => "merging",
            Self::Waiting => "waiting",
        }
    }
}

impl fmt::Display for TakeoverPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for TakeoverPhase {
    type Err = crate::error::DomainError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "planning" => Ok(Self::Planning),
            "reviewing" => Ok(Self::Reviewing),
            "fixing" => Ok(Self::Fixing),
            "working" => Ok(Self::Working),
            "merging" => Ok(Self::Merging),
            "waiting" => Ok(Self::Waiting),
            _ => Err(crate::error::DomainError::Other(format!("unknown takeover phase: {s}"))),
        }
    }
}

/// Completion result of a takeover session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TakeoverResult {
    pub session_id: String,
    pub status: TakeoverStatus,
    pub cycles_completed: u32,
    pub final_phase: TakeoverPhase,
    pub error_count: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_roundtrip() {
        for status in [
            TakeoverStatus::Active,
            TakeoverStatus::Paused,
            TakeoverStatus::Completed,
            TakeoverStatus::Failed,
            TakeoverStatus::Cancelled,
        ] {
            let s = status.to_string();
            let parsed: TakeoverStatus = s.parse().unwrap();
            assert_eq!(parsed, status);
        }
    }

    #[test]
    fn phase_roundtrip() {
        for phase in [
            TakeoverPhase::Planning,
            TakeoverPhase::Reviewing,
            TakeoverPhase::Fixing,
            TakeoverPhase::Working,
            TakeoverPhase::Merging,
            TakeoverPhase::Waiting,
        ] {
            let s = phase.to_string();
            let parsed: TakeoverPhase = s.parse().unwrap();
            assert_eq!(parsed, phase);
        }
    }

    #[test]
    fn terminal_statuses() {
        assert!(TakeoverStatus::Completed.is_terminal());
        assert!(TakeoverStatus::Failed.is_terminal());
        assert!(TakeoverStatus::Cancelled.is_terminal());
        assert!(!TakeoverStatus::Active.is_terminal());
        assert!(!TakeoverStatus::Paused.is_terminal());
    }

    #[test]
    fn invalid_status_parse() {
        assert!("bogus".parse::<TakeoverStatus>().is_err());
    }

    #[test]
    fn invalid_phase_parse() {
        assert!("bogus".parse::<TakeoverPhase>().is_err());
    }
}
