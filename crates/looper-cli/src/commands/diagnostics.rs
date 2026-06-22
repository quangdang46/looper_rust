//! CLI commands for diagnostics

use clap::Subcommand;
use crate::client::DaemonAPIClient;
use crate::error::CliError;
use crate::output;

#[derive(Debug, Subcommand)]
pub enum DiagnosticsCommand {
    Status,
}

pub async fn handle(_client: &DaemonAPIClient, _cmd: &DiagnosticsCommand, _json: bool) -> Result<(), CliError> {
    output::print_ok(_json, &format!("diagnostics command"));
    Ok(())
}
