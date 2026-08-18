//! Log streaming commands.

use crate::client::DaemonAPIClient;
use crate::error::CliError;
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum LogsFollowCommand {
    /// Stream logs for a specific run
    Run {
        /// Run ID to follow
        run_id: String,
    },
}

pub async fn handle(_client: &DaemonAPIClient, cmd: &LogsFollowCommand, _json: bool) -> Result<(), CliError> {
    match cmd {
        LogsFollowCommand::Run { run_id } => {
            eprintln!("Following logs for run {run_id}...");
            eprintln!("(SSE log streaming will be connected to the daemon)");
            eprintln!("Press Ctrl+C to stop.");
            // TODO: Connect to SSE endpoint and stream logs
            Err(CliError::unsupported("looper logs-follow run — SSE streaming not yet implemented"))
        }
    }
}
