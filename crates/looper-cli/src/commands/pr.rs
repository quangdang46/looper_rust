use crate::client::DaemonAPIClient;
use crate::error::CliError;
use clap::{Args, Subcommand};

#[derive(Debug, Subcommand)]
pub enum PrCommand {
    List(PrListArgs),
    Show(PrShowArgs),
    Status(PrStatusArgs),
}

#[derive(Debug, Args)]
pub struct PrListArgs {
    #[arg(short, long)]
    pub project: Option<String>,
}

#[derive(Debug, Args)]
pub struct PrShowArgs {
    pub number: i64,
    #[arg(short, long)]
    pub repo: Option<String>,
}

#[derive(Debug, Args)]
pub struct PrStatusArgs {
    pub number: i64,
    #[arg(short, long)]
    pub repo: Option<String>,
}

pub async fn handle(client: &DaemonAPIClient, cmd: &PrCommand, json: bool) -> Result<(), CliError> {
    match cmd {
        PrCommand::List(args) => {
            let projects = if let Some(ref name) = args.project {
                vec![client.get_project(name).await?]
            } else {
                client.list_projects().await?
            };
            let mut prs = Vec::new();
            for proj in &projects {
                let loops = client.list_loops(&proj.name, 0, 100).await?;
                for l in &loops {
                    if l.status == "active" || l.status == "running" {
                        prs.push(serde_json::json!({
                            "project": proj.name,
                            "loop_seq": l.seq,
                            "status": l.status,
                            "type": l.loop_type,
                            "created_at": l.created_at,
                        }));
                    }
                }
            }
            if !json {
                if prs.is_empty() {
                    println!("No active PRs tracked.");
                } else {
                    println!("{:>6} {:>10} {:>8}  {}", "Seq", "Type", "Status", "Project");
                    for e in &prs {
                        println!(
                            "{:>6} {:>10} {:>8}  {}",
                            e["loop_seq"].as_i64().unwrap_or(0),
                            e["type"].as_str().unwrap_or("?"),
                            e["status"].as_str().unwrap_or("?"),
                            e["project"].as_str().unwrap_or("?"),
                        );
                    }
                }
            } else {
                println!("{}", serde_json::to_string_pretty(&prs).unwrap_or_default());
            }
            Ok(())
        }
        PrCommand::Show(args) => {
            let repo = args.repo.as_deref().unwrap_or("quangdang46/test-looper");
            let out = std::process::Command::new("gh")
                .args([
                    "pr",
                    "view",
                    &args.number.to_string(),
                    "--repo",
                    repo,
                    "--json",
                    "number,title,body,state,author,headRefName,baseRefName,createdAt,labels,additions,deletions",
                ])
                .output()
                .map_err(|e| CliError::daemon_lifecycle(format!("gh failed: {}", e)))?;
            if !out.status.success() {
                let stderr = String::from_utf8_lossy(&out.stderr);
                return Err(CliError::daemon_lifecycle(format!("gh: {}", stderr)));
            }
            let stdout = String::from_utf8_lossy(&out.stdout);
            if json {
                println!("{}", stdout);
            } else {
                let v: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_default();
                println!("PR #{} — {}", v["number"], v["title"].as_str().unwrap_or("?"));
                println!("State: {}", v["state"].as_str().unwrap_or("?"));
                if let Some(auth) = v["author"]["login"].as_str() {
                    println!("Author: {}", auth);
                }
                println!(
                    "Branch: {} ← {}",
                    v["baseRefName"].as_str().unwrap_or("?"),
                    v["headRefName"].as_str().unwrap_or("?")
                );
                println!("Created: {}", v["createdAt"].as_str().unwrap_or("?"));
                if let Some(add) = v["additions"].as_i64() {
                    if let Some(del) = v["deletions"].as_i64() {
                        println!("Diff: +{} -{}", add, del);
                    }
                }
            }
            Ok(())
        }
        PrCommand::Status(args) => {
            let repo = args.repo.as_deref().unwrap_or("quangdang46/test-looper");
            let out = std::process::Command::new("gh")
                .args([
                    "pr",
                    "view",
                    &args.number.to_string(),
                    "--repo",
                    repo,
                    "--json",
                    "number,title,state,mergeStateStatus,statusCheckRollup,reviews",
                ])
                .output()
                .map_err(|e| CliError::daemon_lifecycle(format!("gh failed: {}", e)))?;
            if !out.status.success() {
                let stderr = String::from_utf8_lossy(&out.stderr);
                return Err(CliError::daemon_lifecycle(format!("gh: {}", stderr)));
            }
            let stdout = String::from_utf8_lossy(&out.stdout);
            if json {
                println!("{}", stdout);
            } else {
                let v: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_default();
                println!("PR #{} — {}", v["number"], v["title"].as_str().unwrap_or("?"));
                println!("State: {}", v["state"].as_str().unwrap_or("?"));
                println!("Merge status: {}", v["mergeStateStatus"].as_str().unwrap_or("?"));
                if let Some(checks) = v["statusCheckRollup"].as_array() {
                    let total = checks.len();
                    let pass = checks.iter().filter(|c| c["conclusion"].as_str() == Some("SUCCESS")).count();
                    println!("CI: {}/{} passing", pass, total);
                }
                if let Some(reviews) = v["reviews"].as_array() {
                    let approvals = reviews.iter().filter(|r| r["state"].as_str() == Some("APPROVED")).count();
                    if approvals > 0 {
                        println!("✅ {} approval(s)", approvals);
                    }
                }
            }
            Ok(())
        }
    }
}
