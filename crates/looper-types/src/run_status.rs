use crate::error::DomainError;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// The 7 states in a single run's lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Queued,
    Running,
    Success,
    Failed,
    Cancelled,
    Interrupted,
    /// The agent's raw output could not be parsed into a valid structured response.
    ParseFailed,
}

impl RunStatus {
    /// Return `true` for terminal run states.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            RunStatus::Success
                | RunStatus::Failed
                | RunStatus::Cancelled
                | RunStatus::Interrupted
                | RunStatus::ParseFailed
        )
    }

    /// Check whether a transition is allowed.
    pub fn can_transition_to(self, to: RunStatus) -> bool {
        use RunStatus::*;
        matches!(
            (self, to),
            (Queued, Running | Cancelled | Interrupted)
                | (Running, Success | Failed | Cancelled | Interrupted | ParseFailed)
        )
    }

    /// Attempt a transition; returns `Err` if the transition is invalid.
    pub fn transition(self, to: RunStatus) -> Result<RunStatus, DomainError> {
        if self.can_transition_to(to) {
            Ok(to)
        } else {
            Err(DomainError::InvalidRunStatusTransition { from: self, to })
        }
    }

    /// Return the canonical string label.
    pub fn as_str(self) -> &'static str {
        use RunStatus::*;
        match self {
            Queued => "queued",
            Running => "running",
            Success => "success",
            Failed => "failed",
            Cancelled => "cancelled",
            Interrupted => "interrupted",
            ParseFailed => "parse_failed",
        }
    }
}

impl fmt::Display for RunStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for RunStatus {
    type Err = DomainError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        use RunStatus::*;
        match s {
            "queued" => Ok(Queued),
            "running" => Ok(Running),
            "success" => Ok(Success),
            "failed" => Ok(Failed),
            "cancelled" => Ok(Cancelled),
            "interrupted" => Ok(Interrupted),
            "parse_failed" => Ok(ParseFailed),
            _ => Err(DomainError::UnknownRunStatus { value: s.to_string() }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_transitions() {
        assert!(RunStatus::Queued.can_transition_to(RunStatus::Running));
        assert!(RunStatus::Queued.can_transition_to(RunStatus::Cancelled));
        assert!(RunStatus::Running.can_transition_to(RunStatus::Success));
        assert!(RunStatus::Running.can_transition_to(RunStatus::Failed));
    }

    #[test]
    fn test_terminal_has_no_transitions() {
        let terminal = [
            RunStatus::Success,
            RunStatus::Failed,
            RunStatus::Cancelled,
            RunStatus::Interrupted,
            RunStatus::ParseFailed,
        ];
        let all = [
            RunStatus::Queued,
            RunStatus::Running,
            RunStatus::Success,
            RunStatus::Failed,
            RunStatus::Cancelled,
            RunStatus::Interrupted,
            RunStatus::ParseFailed,
        ];
        for from in terminal {
            for to in all {
                assert!(!from.can_transition_to(to), "terminal {:?} → {:?} should be invalid", from, to);
            }
        }
    }

    #[test]
    fn test_transition_returns_err() {
        assert!(RunStatus::Success.transition(RunStatus::Queued).is_err());
        assert!(RunStatus::Queued.transition(RunStatus::Success).is_err());
    }

    #[test]
    fn test_roundtrip_serde() {
        for s in [RunStatus::Queued, RunStatus::Running, RunStatus::ParseFailed] {
            let json = serde_json::to_string(&s).unwrap();
            let back: RunStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(s, back);
        }
    }

    #[test]
    fn test_from_str() {
        assert_eq!("queued".parse(), Ok(RunStatus::Queued));
        assert_eq!("parse_failed".parse(), Ok(RunStatus::ParseFailed));
        assert!("bogus".parse::<RunStatus>().is_err());
    }

    #[test]
    fn test_display() {
        assert_eq!(RunStatus::Queued.to_string(), "queued");
        assert_eq!(RunStatus::ParseFailed.to_string(), "parse_failed");
    }
}
