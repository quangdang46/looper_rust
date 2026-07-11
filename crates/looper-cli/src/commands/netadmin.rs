//! Netadmin — disabled stub (hidden from help).

use crate::client::DaemonAPIClient;
use crate::error::CliError;
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum NetadminCommand {
    Status,
}

pub async fn handle(_client: &DaemonAPIClient, _cmd: &NetadminCommand, _json: bool) -> Result<(), CliError> {
    Err(CliError::unsupported("looper netadmin"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn netadmin_is_unsupported() {
        let client = DaemonAPIClient::new("http://127.0.0.1:7391".into(), None);
        let err = handle(&client, &NetadminCommand::Status, false).await.unwrap_err();
        assert!(err.to_string().contains("unsupported"));
    }
}
