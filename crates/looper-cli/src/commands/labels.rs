//! CLI commands for labels

use clap::Subcommand;
use crate::client::DaemonAPIClient;
use crate::error::CliError;
use crate::output;

#[derive(Debug, Subcommand)]
pub enum LabelsCommand {
    Status,
}

pub async fn handle(_client: &DaemonAPIClient, _cmd: &LabelsCommand, _json: bool) -> Result<(), CliError> {
    output::print_ok(_json, &format!("labels command"));
    Ok(())
}
