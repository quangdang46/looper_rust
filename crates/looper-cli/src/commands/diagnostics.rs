use clap::Subcommand;
use crate::client::DaemonAPIClient;
use crate::error::CliError;
use crate::output;

#[derive(Debug, Subcommand)]
pub enum DiagnosticsCommand {
    Status,
}

pub async fn handle(client: &DaemonAPIClient, cmd: &DiagnosticsCommand, json: bool) -> Result<(), CliError> {
    match cmd {
        DiagnosticsCommand::Status => {
            let s = client.health().await?;
            output::print_output(json, &s);
        }
    }
    Ok(())
}
