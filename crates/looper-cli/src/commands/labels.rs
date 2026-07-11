//! Labels — disabled stub (hidden from help).

use crate::client::DaemonAPIClient;
use crate::error::CliError;
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum LabelsCommand {
    Status,
}

pub async fn handle(_client: &DaemonAPIClient, _cmd: &LabelsCommand, _json: bool) -> Result<(), CliError> {
    Err(CliError::unsupported("looper labels"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn labels_is_unsupported() {
        let client = DaemonAPIClient::new("http://127.0.0.1:7391".into(), None);
        let err = handle(&client, &LabelsCommand::Status, false).await.unwrap_err();
        assert!(err.to_string().contains("unsupported"));
    }
}
