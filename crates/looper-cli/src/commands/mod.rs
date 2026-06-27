pub mod bootstrap;
pub mod config;
pub mod diagnostics;
pub mod events;
pub mod feedback;
pub mod jump;
pub mod labels;
pub mod locks;
pub mod logs_follow;
pub mod loops;
pub mod netadmin;
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
pub mod worktree;

use crate::client::DaemonAPIClient;
use crate::error::CliError;

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
    let base = daemon_url.unwrap_or("http://127.0.0.1:8080");
    Ok(DaemonAPIClient::new(base.to_string(), token.map(|s| s.to_string())))
}

pub async fn ensure_daemon(client: &DaemonAPIClient) -> Result<(), CliError> {
    if !client.ping().await {
        return Err(CliError::daemon_not_running());
    }
    Ok(())
}
