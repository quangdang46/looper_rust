//! CLI commands for logs_follow

use clap::Subcommand;
use crate::client::DaemonAPIClient;
use crate::error::CliError;
use crate::output;

#[derive(Debug, Subcommand)]
pub enum LogsFollowCommand {
    Status,
}

pub async fn handle(_client: &DaemonAPIClient, _cmd: &LogsFollowCommand, _json: bool) -> Result<(), CliError> {
    output::print_ok(_json, &format!("logs_follow command"));
    Ok(())
}
