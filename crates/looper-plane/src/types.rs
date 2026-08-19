//! Plane-specific types and conversions.

use looper_forge::{Issue, IssueState};
use serde::Deserialize;

/// Raw Plane issue from the API.
#[derive(Debug, Clone, Deserialize)]
pub struct PlaneIssue {
    pub id: String,
    pub identifier: String,
    pub name: String,
    pub description: Option<String>,
    pub state: String,
    pub priority: Option<String>,
    pub assignees: Option<Vec<String>>,
    pub labels: Option<Vec<String>>,
    pub created_at: String,
    pub updated_at: String,
    pub created_by: Option<String>,
}

/// Raw Plane API response for issues list.
#[derive(Debug, Deserialize)]
pub(crate) struct PlaneIssuesResponse {
    pub results: Vec<PlaneIssue>,
}

/// Convert a Plane issue into a looper-forge Issue.
pub fn issue_from_plane(p: PlaneIssue) -> Issue {
    let number = p.identifier.split('-').next_back().and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);

    let state = match p.state.to_lowercase().as_str() {
        "backlog" | "todo" | "in_progress" | "in review" => IssueState::Open,
        "done" | "cancelled" => IssueState::Closed,
        _ => IssueState::Open,
    };

    Issue {
        number,
        title: p.name,
        body: p.description,
        state,
        labels: p.labels.unwrap_or_default(),
        assignees: p.assignees.unwrap_or_default(),
        author: p.created_by.unwrap_or_default(),
        created_at: p.created_at,
        updated_at: p.updated_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plane_issue_parsing() {
        let plane_issue = PlaneIssue {
            id: "uuid-123".to_string(),
            identifier: "MYPROJ-42".to_string(),
            name: "Fix the login bug".to_string(),
            description: Some("Users cannot log in with SSO".to_string()),
            state: "in_progress".to_string(),
            priority: Some("high".to_string()),
            assignees: Some(vec!["alice".to_string()]),
            labels: Some(vec!["bug".to_string(), "auth".to_string()]),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-02T00:00:00Z".to_string(),
            created_by: Some("admin".to_string()),
        };

        let issue = issue_from_plane(plane_issue);
        assert_eq!(issue.number, 42);
        assert_eq!(issue.title, "Fix the login bug");
        assert_eq!(issue.state, IssueState::Open);
        assert_eq!(issue.labels, vec!["bug", "auth"]);
        assert_eq!(issue.assignees, vec!["alice"]);
        assert_eq!(issue.author, "admin");
    }

    #[test]
    fn plane_issue_closed_state() {
        let plane_issue = PlaneIssue {
            id: "uuid-456".to_string(),
            identifier: "MYPROJ-10".to_string(),
            name: "Done task".to_string(),
            description: None,
            state: "done".to_string(),
            priority: None,
            assignees: None,
            labels: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-02T00:00:00Z".to_string(),
            created_by: None,
        };

        let issue = issue_from_plane(plane_issue);
        assert_eq!(issue.number, 10);
        assert_eq!(issue.state, IssueState::Closed);
    }

    #[test]
    fn plane_issue_identifier_parsing() {
        let plane_issue = PlaneIssue {
            id: "uuid-789".to_string(),
            identifier: "PROJ-1".to_string(),
            name: "First issue".to_string(),
            description: None,
            state: "backlog".to_string(),
            priority: None,
            assignees: None,
            labels: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            created_by: None,
        };

        let issue = issue_from_plane(plane_issue);
        assert_eq!(issue.number, 1);
        assert_eq!(issue.state, IssueState::Open);
    }
}
