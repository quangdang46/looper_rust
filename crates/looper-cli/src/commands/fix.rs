//! `looper fix` — admit fixer work for a PR (Go-compatible surface).

use clap::Args;

use crate::client::DaemonAPIClient;
use crate::error::CliError;

use super::work::{admit_role, RoleIssueArgs};

#[derive(Debug, Args)]
pub struct FixArgs {
    #[command(flatten)]
    pub common: RoleIssueArgs,
}

pub async fn handle(client: &DaemonAPIClient, args: &FixArgs, json: bool) -> Result<(), CliError> {
    admit_role(
        client,
        &args.common.project,
        "fixer",
        args.common.issue,
        args.common.pr,
        args.common.repo.clone(),
        args.common.priority,
        json,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Debug, Parser)]
    struct Wrap {
        #[command(flatten)]
        args: FixArgs,
    }

    #[test]
    fn parse_fix_pr() {
        let w = Wrap::try_parse_from(["t", "--project", "p", "--pr", "11"]).unwrap();
        assert_eq!(w.args.common.pr, Some(11));
    }
}
