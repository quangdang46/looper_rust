//! Run statistics — disabled stub (hidden from help).

use crate::client::DaemonAPIClient;
use crate::error::CliError;
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum RunStatsCommand {
    Show { run_id: String },
    Timeline { run_id: String },
    History { limit: Option<usize> },
}

pub async fn handle(_client: &DaemonAPIClient, _cmd: &RunStatsCommand, _json: bool) -> Result<(), CliError> {
    Err(CliError::unsupported("looper run-stats"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn run_stats_is_unsupported() {
        let client = DaemonAPIClient::new("http://127.0.0.1:7391".into(), None);
        let err = handle(&client, &RunStatsCommand::Show { run_id: "r1".into() }, false).await.unwrap_err();
        assert!(err.to_string().contains("unsupported"));
    }
}
