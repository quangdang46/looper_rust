//! CLI commands for feedback

use clap::Subcommand;
use crate::client::DaemonAPIClient;
use crate::error::CliError;
use crate::output;

#[derive(Debug, Subcommand)]
pub enum FeedbackCommand {
    Status,
}

pub async fn handle(_client: &DaemonAPIClient, _cmd: &FeedbackCommand, _json: bool) -> Result<(), CliError> {
    output::print_ok(_json, &format!("feedback command"));
    Ok(())
}
