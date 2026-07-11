pub mod bootstrap;
pub mod config;
pub mod diagnostics;
pub mod events;
pub mod feedback;
pub mod fix;
pub mod jump;
pub mod labels;
pub mod locks;
pub mod logs_follow;
pub mod loops;
pub mod netadmin;
pub mod plan;
pub mod pr;
pub mod projects;
pub mod prompt;
pub mod ps;
pub mod queue;
pub mod reconcile;
pub mod review;
pub mod run_stats;
pub mod runs;
pub mod stop;
pub mod takeover;
pub mod webhook;
pub mod work;
pub mod worktree;

use crate::client::DaemonAPIClient;
use crate::error::CliError;

/// Default daemon REST base URL — must match looperd / looper-config default port 7391.
pub const DEFAULT_DAEMON_BASE_URL: &str = "http://127.0.0.1:7391";

/// Resolve a project name from an optional argument. If not provided
/// and there's exactly one project, use that. Otherwise error.
pub async fn resolve_project(client: &DaemonAPIClient, project: &Option<String>) -> Result<String, CliError> {
    if let Some(p) = project {
        return Ok(p.clone());
    }
    let projects = client.list_projects().await?;
    if projects.len() == 1 {
        return Ok(projects[0].name.clone());
    }
    Err(CliError::daemon_lifecycle("--project is required when multiple projects exist"))
}

pub fn build_client(daemon_url: Option<&str>, token: Option<&str>) -> Result<DaemonAPIClient, CliError> {
    let base = daemon_url.unwrap_or(DEFAULT_DAEMON_BASE_URL);
    Ok(DaemonAPIClient::new(base.to_string(), token.map(|s| s.to_string())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_daemon_base_url_is_7391() {
        assert_eq!(DEFAULT_DAEMON_BASE_URL, "http://127.0.0.1:7391");
        assert!(!DEFAULT_DAEMON_BASE_URL.contains("8080"));
    }

    #[test]
    fn build_client_defaults_to_7391() {
        let client = build_client(None, None).expect("build client");
        assert_eq!(client.base_url(), DEFAULT_DAEMON_BASE_URL);
    }

    #[test]
    fn build_client_respects_override() {
        let client = build_client(Some("http://127.0.0.1:9999"), None).expect("build client");
        assert_eq!(client.base_url(), "http://127.0.0.1:9999");
    }
}

pub async fn ensure_daemon(client: &DaemonAPIClient) -> Result<(), CliError> {
    if !client.ping().await {
        return Err(CliError::daemon_not_running());
    }
    Ok(())
}
