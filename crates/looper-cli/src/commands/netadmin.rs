use crate::client::DaemonAPIClient;
use crate::error::CliError;
use crate::output;
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum NetadminCommand {
    Status,
}

pub async fn handle(client: &DaemonAPIClient, cmd: &NetadminCommand, json: bool) -> Result<(), CliError> {
    match cmd {
        NetadminCommand::Status => {
            let s = client.health().await?;
            output::print_output(json, &s);
        }
    }
    Ok(())
}
