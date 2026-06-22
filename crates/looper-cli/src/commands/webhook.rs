//! CLI commands for webhook

use clap::Subcommand;
use crate::client::DaemonAPIClient;
use crate::error::CliError;
use crate::output;

#[derive(Debug, Subcommand)]
pub enum WebhookCommand {
    Status,
}

pub async fn handle(_client: &DaemonAPIClient, _cmd: &WebhookCommand, _json: bool) -> Result<(), CliError> {
    output::print_ok(_json, &format!("webhook command"));
    Ok(())
}
