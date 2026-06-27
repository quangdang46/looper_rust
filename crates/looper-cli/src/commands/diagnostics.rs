use crate::client::DaemonAPIClient;
use crate::error::CliError;
use crate::output;
use clap::Subcommand;

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
