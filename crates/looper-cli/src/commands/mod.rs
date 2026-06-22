pub mod config;
pub mod events;
pub mod locks;
pub mod loops;
pub mod projects;
pub mod queue;
pub mod review;
pub mod runs;
pub mod takeover;

use crate::client::DaemonAPIClient;
use crate::error::CliError;

/// Create a client from CLI global flags.
pub fn build_client(
    daemon_url: Option<&str>,
    token: Option<&str>,
) -> Result<DaemonAPIClient, CliError> {
    let base = daemon_url.unwrap_or("http://127.0.0.1:8080");
    let t = token.map(|s| s.to_string());
    Ok(DaemonAPIClient::new(base.to_string(), t))
}

/// Ensure the daemon is reachable before running a command.
pub async fn ensure_daemon(client: &DaemonAPIClient) -> Result<(), CliError> {
    if !client.ping().await {
        return Err(CliError::daemon_not_running());
    }
    Ok(())
}
