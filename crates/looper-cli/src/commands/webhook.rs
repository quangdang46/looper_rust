//! Webhook — disabled stub (hidden from help).

use crate::client::DaemonAPIClient;
use crate::error::CliError;
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum WebhookCommand {
    Status,
}

pub async fn handle(_client: &DaemonAPIClient, _cmd: &WebhookCommand, _json: bool) -> Result<(), CliError> {
    Err(CliError::unsupported("looper webhook"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn webhook_is_unsupported() {
        let client = DaemonAPIClient::new("http://127.0.0.1:7391".into(), None);
        let err = handle(&client, &WebhookCommand::Status, false).await.unwrap_err();
        assert!(err.to_string().contains("unsupported"));
    }
}
