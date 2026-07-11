//! `looper plan` — admit planner work for an issue (Go-compatible surface).

use clap::Args;

use crate::client::DaemonAPIClient;
use crate::error::CliError;

use super::work::{admit_role, RoleIssueArgs};

#[derive(Debug, Args)]
pub struct PlanArgs {
    #[command(flatten)]
    pub common: RoleIssueArgs,
}

pub async fn handle(client: &DaemonAPIClient, args: &PlanArgs, json: bool) -> Result<(), CliError> {
    admit_role(
        client,
        &args.common.project,
        "planner",
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
        args: PlanArgs,
    }

    #[test]
    fn parse_plan_issue() {
        let w = Wrap::try_parse_from(["t", "--project", "p", "--issue", "42"]).unwrap();
        assert_eq!(w.args.common.issue, Some(42));
        assert_eq!(w.args.common.project, "p");
    }
}
