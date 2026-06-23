pub mod config;
pub mod diagnostics;
pub mod events;
pub mod feedback;
pub mod labels;
pub mod locks;
pub mod logs_follow;
pub mod loops;
pub mod netadmin;
pub mod projects;
pub mod prompt;
pub mod queue;
pub mod review;
pub mod run_stats;
pub mod runs;
pub mod takeover;
pub mod webhook;
pub mod worktree;

use crate::client::DaemonAPIClient;
use crate::error::CliError;

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
