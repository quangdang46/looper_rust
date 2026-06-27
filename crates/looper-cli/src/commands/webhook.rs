use crate::client::DaemonAPIClient;
use crate::error::CliError;
use crate::output;
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum WebhookCommand {
    Status,
}

pub async fn handle(client: &DaemonAPIClient, cmd: &WebhookCommand, json: bool) -> Result<(), CliError> {
    match cmd {
        WebhookCommand::Status => {
            let s = client.health().await?;
            output::print_output(json, &s);
        }
    }
    Ok(())
}
