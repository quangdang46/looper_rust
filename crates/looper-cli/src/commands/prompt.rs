//! CLI commands for prompt

use clap::Subcommand;
use crate::client::DaemonAPIClient;
use crate::error::CliError;
use crate::output;

#[derive(Debug, Subcommand)]
pub enum PromptCommand {
    Status,
}

pub async fn handle(_client: &DaemonAPIClient, _cmd: &PromptCommand, _json: bool) -> Result<(), CliError> {
    output::print_ok(_json, &format!("prompt command"));
    Ok(())
}
