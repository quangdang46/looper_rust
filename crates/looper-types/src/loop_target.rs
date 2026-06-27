use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

use crate::error::DomainError;

/// The kind of entity a loop targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopTargetType {
    Project,
    PullRequest,
    Issue,
}

impl LoopTargetType {
    /// Return the canonical string label.
    pub fn as_str(self) -> &'static str {
        use LoopTargetType::*;
        match self {
            Project => "project",
            PullRequest => "pull_request",
            Issue => "issue",
        }
    }
}

impl fmt::Display for LoopTargetType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for LoopTargetType {
    type Err = DomainError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        use LoopTargetType::*;
        match s {
            "project" => Ok(Project),
            "pull_request" => Ok(PullRequest),
            "issue" => Ok(Issue),
            _ => Err(DomainError::UnknownLoopTargetType { value: s.to_string() }),
        }
    }
}

/// Identifies what a loop operates on.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LoopTarget {
    pub target_type: LoopTargetType,
    /// `project_id` is used for `Project`-type targets.
    pub project_id: Option<String>,
    /// `repo` is used for PR/issue targets (e.g., `owner/repo`).
    pub repo: Option<String>,
    /// `number` is the PR or issue number.
    pub number: Option<i64>,
}

/// Compute the unique target key for a loop target.
///
/// Format:
/// - Project:    `project:{project_id}`
/// - PullRequest: `pr:{repo}#{number}`
/// - Issue:      `issue:{repo}#{number}`
pub fn loop_target_key(target: &LoopTarget) -> String {
    match target.target_type {
        LoopTargetType::Project => {
            let id = target.project_id.as_deref().unwrap_or("?");
            format!("project:{}", id)
        }
        LoopTargetType::PullRequest => {
            let repo = target.repo.as_deref().unwrap_or("?");
            let num = target.number.unwrap_or(0);
            format!("pr:{}#{}", repo, num)
        }
        LoopTargetType::Issue => {
            let repo = target.repo.as_deref().unwrap_or("?");
            let num = target.number.unwrap_or(0);
            format!("issue:{}#{}", repo, num)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_target_key_project() {
        let target = LoopTarget {
            target_type: LoopTargetType::Project,
            project_id: Some("my-project".into()),
            repo: None,
            number: None,
        };
        assert_eq!(loop_target_key(&target), "project:my-project");
    }

    #[test]
    fn test_target_key_pr() {
        let target = LoopTarget {
            target_type: LoopTargetType::PullRequest,
            project_id: None,
            repo: Some("owner/repo".into()),
            number: Some(42),
        };
        assert_eq!(loop_target_key(&target), "pr:owner/repo#42");
    }

    #[test]
    fn test_target_key_issue() {
        let target = LoopTarget {
            target_type: LoopTargetType::Issue,
            project_id: None,
            repo: Some("owner/repo".into()),
            number: Some(7),
        };
        assert_eq!(loop_target_key(&target), "issue:owner/repo#7");
    }

    #[test]
    fn test_target_key_fallback() {
        let target = LoopTarget { target_type: LoopTargetType::Project, project_id: None, repo: None, number: None };
        assert_eq!(loop_target_key(&target), "project:?");
    }

    #[test]
    fn test_roundtrip_serde() {
        let target = LoopTarget {
            target_type: LoopTargetType::PullRequest,
            project_id: None,
            repo: Some("org/repo".into()),
            number: Some(100),
        };
        let json = serde_json::to_string(&target).unwrap();
        let back: LoopTarget = serde_json::from_str(&json).unwrap();
        assert_eq!(target, back);
    }

    #[test]
    fn test_from_str_loop_target_type() {
        assert_eq!("project".parse(), Ok(LoopTargetType::Project));
        assert_eq!("pull_request".parse(), Ok(LoopTargetType::PullRequest));
        assert_eq!("issue".parse(), Ok(LoopTargetType::Issue));
        assert!("bogus".parse::<LoopTargetType>().is_err());
    }

    #[test]
    fn test_display_loop_target_type() {
        assert_eq!(LoopTargetType::Project.to_string(), "project");
        assert_eq!(LoopTargetType::PullRequest.to_string(), "pull_request");
    }
}
