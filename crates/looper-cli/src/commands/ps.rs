use crate::client::DaemonAPIClient;
use crate::error::CliError;
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum PsCommand {
    List,
}

pub async fn handle(client: &DaemonAPIClient, cmd: &PsCommand, json: bool) -> Result<(), CliError> {
    match cmd {
        PsCommand::List => {
            let projects = client.list_projects().await?;
            let mut all_loops = Vec::new();
            for proj in &projects {
                let loops = client.list_loops(&proj.name, 0, 100).await?;
                for l in &loops {
                    if l.status == "active" || l.status == "running" {
                        all_loops.push(serde_json::json!({
                            "project": proj.name,
                            "seq": l.seq,
                            "type": l.loop_type,
                            "status": l.status,
                            "created_at": l.created_at,
                        }));
                    }
                }
            }
            if !json {
                if all_loops.is_empty() {
                    println!("No active loops.");
                } else {
                    println!("{:>4} {:>6} {:>10} {:>8}  {}", "Seq", "Type", "Status", "Project", "Created");
                    for e in &all_loops {
                        println!(
                            "{:>4} {:>6} {:>10} {:>8}  {}",
                            e["seq"].as_i64().unwrap_or(0),
                            e["type"].as_str().unwrap_or("?"),
                            e["status"].as_str().unwrap_or("?"),
                            e["project"].as_str().unwrap_or("?"),
                            e["created_at"].as_str().unwrap_or("?"),
                        );
                    }
                }
            } else {
                println!("{}", serde_json::to_string_pretty(&all_loops).unwrap_or_default());
            }
            Ok(())
        }
    }
}
