//! Feedback — disabled stub (hidden from help).

use crate::client::DaemonAPIClient;
use crate::error::CliError;
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum FeedbackCommand {
    Status,
}

pub async fn handle(_client: &DaemonAPIClient, _cmd: &FeedbackCommand, _json: bool) -> Result<(), CliError> {
    Err(CliError::unsupported("looper feedback"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn feedback_is_unsupported() {
        let client = DaemonAPIClient::new("http://127.0.0.1:7391".into(), None);
        let err = handle(&client, &FeedbackCommand::Status, false).await.unwrap_err();
        assert!(err.to_string().contains("unsupported"));
    }
}
