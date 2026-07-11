//! Diagnostics — disabled stub (hidden until real implementation).

use crate::client::DaemonAPIClient;
use crate::error::CliError;
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum DiagnosticsCommand {
    Status,
}

pub async fn handle(_client: &DaemonAPIClient, _cmd: &DiagnosticsCommand, _json: bool) -> Result<(), CliError> {
    Err(CliError::unsupported("looper diagnostics"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn diagnostics_is_unsupported() {
        let client = DaemonAPIClient::new("http://127.0.0.1:7391".into(), None);
        let err = handle(&client, &DiagnosticsCommand::Status, false).await.unwrap_err();
        assert!(err.to_string().contains("unsupported"));
    }
}
