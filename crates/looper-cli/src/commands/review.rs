//! Review commands — disabled stub (hidden from help).

use crate::client::DaemonAPIClient;
use crate::error::CliError;
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum ReviewCommand {
    Submit {
        pr: i64,
        #[arg(long)]
        body: Option<String>,
        #[arg(long, default_value = "comment")]
        event: String,
    },
    Status {
        pr: i64,
    },
}

pub async fn handle(_client: &DaemonAPIClient, _cmd: &ReviewCommand, _json: bool) -> Result<(), CliError> {
    Err(CliError::unsupported("looper review"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn review_is_unsupported() {
        let client = DaemonAPIClient::new("http://127.0.0.1:7391".into(), None);
        let err = handle(&client, &ReviewCommand::Status { pr: 1 }, false).await.unwrap_err();
        assert!(err.to_string().contains("unsupported"));
    }
}
