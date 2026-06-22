//! CLI commands for netadmin

use clap::Subcommand;
use crate::client::DaemonAPIClient;
use crate::error::CliError;
use crate::output;

#[derive(Debug, Subcommand)]
pub enum NetadminCommand {
    Status,
}

pub async fn handle(_client: &DaemonAPIClient, _cmd: &NetadminCommand, _json: bool) -> Result<(), CliError> {
    output::print_ok(_json, &format!("netadmin command"));
    Ok(())
}
