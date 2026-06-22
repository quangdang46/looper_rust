use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

use crate::error::DomainError;

/// Classifies why a run failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    /// A transient issue that should be retried automatically.
    RetryableTransient,
    /// A retryable failure that requires manual resume first.
    RetryableAfterResume,
    /// A non-recoverable failure.
    NonRetryable,
    /// Requires human intervention.
    ManualIntervention,
}

impl FailureKind {
    /// Return the canonical string label.
    pub fn as_str(self) -> &'static str {
        use FailureKind::*;
        match self {
            RetryableTransient => "retryable_transient",
            RetryableAfterResume => "retryable_after_resume",
            NonRetryable => "non_retryable",
            ManualIntervention => "manual_intervention",
        }
    }
}

impl fmt::Display for FailureKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for FailureKind {
    type Err = DomainError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        use FailureKind::*;
        match s {
            "retryable_transient" => Ok(RetryableTransient),
            "retryable_after_resume" => Ok(RetryableAfterResume),
            "non_retryable" => Ok(NonRetryable),
            "manual_intervention" => Ok(ManualIntervention),
            _ => Err(DomainError::UnknownFailureKind { value: s.to_string() }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_roundtrip_serde() {
        for fk in [FailureKind::RetryableTransient, FailureKind::ManualIntervention] {
            let json = serde_json::to_string(&fk).unwrap();
            let back: FailureKind = serde_json::from_str(&json).unwrap();
            assert_eq!(fk, back);
        }
    }

    #[test]
    fn test_from_str() {
        assert_eq!("non_retryable".parse(), Ok(FailureKind::NonRetryable));
        assert!("unknown".parse::<FailureKind>().is_err());
    }
}
