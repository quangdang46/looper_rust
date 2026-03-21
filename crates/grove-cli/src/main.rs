use anyhow::{Context, Result, anyhow, bail};
use camino::Utf8PathBuf;
use clap::{ArgAction, Parser, Subcommand};
use serde_json::json;
use grove_br::{BrClient, BrIssueDetail, CliBrClient, sync_bead_cache};
use grove_bv::{BvClient, BvTriageOutput, CliBvClient};
use grove_config::{
    GroveConfig, GrovePaths, LoadedConfig, RequiredTooling, ToolCapability,
    detect_required_tooling, load_from_workspace,
};
use grove_db::Database;
use grove_kernel::{
    BeadInspectView, LeaderLeaseConfig, LeaderLeaseManager, ShutdownSignal, StartupRecoveryReport,
    WorkspaceStatusView, acquire_startup_coordinator, load_bead_inspect_view,
    load_workspace_status_view, run_dispatch_loop, DispatchLoopConfig,
};
use grove_types::{BeadId, BeadPriority, GroveBeadStatus, LeaderLeaseRecord};
use std::{cmp, env, fs};

#[derive(Parser)]
#[command(name = "grove")]
#[command(about = "Autonomous orchestration for beads-backed Claude work")]
struct Cli {
    #[arg(long, global = true, action = ArgAction::SetTrue)]
    json: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    Init,
    Status,
    Inspect { bead_id: String },
    Log { bead_id: String },
    Retry { bead_id: String },
    Run,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Init) => run_json_command("init", cli.json, || handle_init(cli.json)),
        Some(Command::Status) => handle_status(cli.json),
        Some(Command::Inspect { bead_id }) => handle_inspect(&BeadId::new(bead_id), cli.json),
        Some(Command::Log { bead_id }) => handle_log(&BeadId::new(bead_id), cli.json),
        Some(Command::Retry { bead_id }) => handle_retry(&BeadId::new(bead_id), cli.json),
        Some(Command::Run) => run_json_command("run", cli.json, || handle_run(cli.json)),
        None => {
            if cli.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "ok": true,
                        "command": null,
                        "message": "Use `grove --help` to see available commands.",
                        "available_commands": ["init", "status", "inspect", "log", "retry", "run"],
                    }))?
                );
            } else {
                println!("Use `grove --help` to see available commands.");
            }
            Ok(())
        }
    }
}

fn run_json_command<F>(command: &str, json_mode: bool, op: F) -> Result<()>
where
    F: FnOnce() -> Result<()>,
{
    match op() {
        Ok(()) => Ok(()),
        Err(error) if json_mode => {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "ok": false,
                    "command": command,
                    "error": format_error_chain(&error),
                }))?
            );
            Ok(())
        }
        Err(error) => Err(error),
    }
}

fn format_error_chain(error: &anyhow::Error) -> Vec<String> {
    error.chain().map(|cause| cause.to_string()).collect()
}

fn handle_init(json_mode: bool) -> Result<()> {
    let workspace_root = current_workspace_root()?;
    let config_path = workspace_root.join("grove.toml");
    let wrote_default_config = if config_path.exists() {
        false
    } else {
        write_default_config(&config_path)?;
        true
    };

    let loaded = load_from_workspace(&workspace_root).with_context(|| {
        format!(
            "load grove configuration from {}",
            workspace_root.join("grove.toml")
        )
    })?;
    ensure_workspace_layout(&loaded.paths)?;

    let tooling = detect_required_tooling(&loaded.config);
    ensure_required_tooling(&tooling)?;

    let br = CliBrClient::new("br", loaded.paths.workspace_root().as_str());
    let bv = CliBvClient::new("bv", loaded.paths.workspace_root().as_str());
    let br_capability = br.capability().context("check br capability")?;
    let bv_capability = bv.capability().context("check bv capability")?;

    let mut db = Database::open(loaded.paths.db_path())
        .with_context(|| format!("open database at {}", loaded.paths.db_path()))?;
    db.migrate().context("apply database migrations")?;

    let synced_beads = if br_capability.beads_dir_exists {
        let sync_result = sync_bead_cache(&br, &mut db).context("sync bead cache from br")?;
        sync_result.beads_synced
    } else {
        0
    };

    if json_mode {
        let notes = [
            (!br_capability.beads_dir_exists).then_some(
                "No .beads directory detected yet; run `br init` before `grove status` or `grove run`.",
            ),
            (!bv_capability.beads_dir_exists).then_some(
                "`bv` does not see a .beads directory yet, so triage commands will not be available until beads are initialized.",
            ),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();

        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "ok": true,
                "workspace_root": loaded.paths.workspace_root().as_str(),
                "db_path": loaded.paths.db_path().as_str(),
                "config_path": loaded.paths.config_path().as_str(),
                "transcript_dir": loaded.paths.transcript_dir().as_str(),
                "checkpoints_dir": loaded.paths.checkpoints_dir().as_str(),
                "wrote_default_config": wrote_default_config,
                "synced_beads": synced_beads,
                "tooling": {
                    "claude": render_tool_line(&tooling.claude.binary, tooling.claude.version.as_deref()),
                    "br": render_tool_line("br", br_capability.version_line.as_deref()),
                    "bv": render_tool_line("bv", bv_capability.version.as_deref()),
                },
                "notes": notes,
                "next_steps": [
                    "Create or review beads with br",
                    "Run grove status",
                    "Run grove inspect <bead-id>",
                ],
            }))?
        );
        return Ok(());
    }

    println!("Initialized grove workspace.");
    println!("- database: {}", loaded.paths.db_path());
    println!("- config: {}", loaded.paths.config_path());
    println!("- transcripts: {}", loaded.paths.transcript_dir());
    println!("- checkpoints: {}", loaded.paths.checkpoints_dir());
    if wrote_default_config {
        println!("- wrote default config: {}", loaded.paths.config_path());
    }
    if br_capability.beads_dir_exists {
        println!("- bead cache synced: {synced_beads} bead(s)");
    }

    println!("\nValidated tools:");
    println!(
        "- {}",
        render_tool_line(&tooling.claude.binary, tooling.claude.version.as_deref())
    );
    println!(
        "- {}",
        render_tool_line("br", br_capability.version_line.as_deref())
    );
    println!(
        "- {}",
        render_tool_line("bv", bv_capability.version.as_deref())
    );

    if !br_capability.beads_dir_exists || !bv_capability.beads_dir_exists {
        println!("\nNotes:");
        if !br_capability.beads_dir_exists {
            println!(
                "- No .beads directory detected yet; run `br init` before `grove status` or `grove run`."
            );
        }
        if !bv_capability.beads_dir_exists {
            println!(
                "- `bv` does not see a .beads directory yet, so triage commands will not be available until beads are initialized."
            );
        }
    }

    println!("\nNext steps:");
    println!("1. Create or review beads with `br`");
    println!("2. Run `grove status`");
    println!("3. Run `grove inspect <bead-id>`");

    Ok(())
}

fn handle_status(json_mode: bool) -> Result<()> {
    let (loaded, db, br) = open_runtime()?;
    let bv = CliBvClient::new("bv", loaded.paths.workspace_root().as_str());
    let (triage, triage_error) = match bv.triage() {
        Ok(output) => (Some(output), None),
        Err(error) => (None, Some(error.to_string())),
    };

    let view = load_workspace_status_view(
        &db,
        &br,
        loaded.paths.workspace_root().as_str(),
        &loaded.config,
        triage.as_ref(),
    )
    .context("load workspace status view")?;

    if json_mode {
        let triage_summary = triage.as_ref().map(|triage| {
            json!({
                "generated_at": triage.generated_at,
                "data_hash": triage.data_hash,
                "quick_ref": {
                    "open_count": triage.quick_ref.open_count,
                    "actionable_count": triage.quick_ref.actionable_count,
                    "blocked_count": triage.quick_ref.blocked_count,
                    "in_progress_count": triage.quick_ref.in_progress_count,
                    "top_pick_ids": triage
                        .quick_ref
                        .top_picks
                        .iter()
                        .map(|pick| pick.id.as_str())
                        .collect::<Vec<_>>(),
                }
            })
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "workspace_root": loaded.paths.workspace_root().as_str(),
                "db_path": loaded.paths.db_path().as_str(),
                "view": view,
                "triage_summary": triage_summary,
                "triage_error": triage_error,
            }))?
        );
        return Ok(());
    }

    print_status_view(
        &view,
        loaded.paths.db_path().as_str(),
        triage.as_ref(),
        triage_error.as_deref(),
    );
    Ok(())
}

fn handle_inspect(bead_id: &BeadId, json_mode: bool) -> Result<()> {
    let (loaded, db, br) = open_runtime()?;
    let bv = CliBvClient::new("bv", loaded.paths.workspace_root().as_str());
    let triage = bv.triage().ok();
    let issue_detail = br.show(bead_id).ok();
    let view = load_bead_inspect_view(
        &db,
        &br,
        bead_id,
        loaded.paths.workspace_root().as_str(),
        &loaded.config,
        triage.as_ref(),
    )
    .with_context(|| format!("load inspect view for {bead_id}"))?;

    if issue_detail.is_none() && view.is_none() {
        bail!("bead {bead_id} was not found in br or the local Grove cache");
    }

    if json_mode {
        let issue_summary = issue_detail.as_ref().map(|detail| {
            json!({
                "id": detail.summary.id.as_str(),
                "title": detail.summary.title,
                "status": detail.summary.status,
                "issue_type": detail.summary.issue_type,
                "priority": format_priority(detail.summary.priority),
                "labels": detail.summary.labels,
                "assignee": detail.summary.assignee,
                "description": detail.summary.description,
                "closed_at": detail.closed_at,
                "close_reason": detail.close_reason,
                "comment_count": detail.comments.len(),
            })
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "workspace_root": loaded.paths.workspace_root().as_str(),
                "bead_id": bead_id.as_str(),
                "issue_summary": issue_summary,
                "view": view,
            }))?
        );
        return Ok(());
    }

    print_inspect_report(
        loaded.paths.workspace_root().as_str(),
        issue_detail.as_ref(),
        view.as_ref(),
    );
    Ok(())
}

fn handle_run(json_mode: bool) -> Result<()> {
    let (loaded, mut db, br) = open_runtime()?;
    let owner_label = format!("{}:{}", loaded.paths.workspace_root(), std::process::id());
    let lease_ttl = chrono::Duration::milliseconds(
        cmp::max(1, loaded.config.scheduler.poll_interval_ms as i64) * 2,
    );
    let lease_config = LeaderLeaseConfig {
        owner_label,
        lease_ttl,
    };
    let now = chrono::Utc::now();
    let startup = acquire_startup_coordinator(&mut db, &lease_config, None, now)
        .map_err(|error| anyhow!(error.to_string()))?;

    run_startup_checks(&mut db, &lease_config, startup)?;
    if !json_mode {
        println!("Startup recovery checks complete. Beginning dispatch loop.");
    }

    // Create and register shutdown signal for graceful Ctrl-C handling.
    let shutdown_signal = ShutdownSignal::new();
    shutdown_signal
        .register_ctrlc()
        .context("register shutdown handler")?;

    let backend = grove_session::CliClaudeBackend::new(loaded.config.runtime.claude_bin.clone());
    let loop_config = DispatchLoopConfig {
        max_total_runs: None,
        max_poll_cycles: None,
        working_dir: loaded.paths.workspace_root().to_owned(),
        shutdown_signal,
        db_path: loaded.paths.db_path().to_owned(),
    };

    let dispatch_result = run_dispatch_loop(
        &mut db,
        &backend,
        &br,
        &loaded.config,
        &lease_config,
        &loop_config,
    );

    let release_at = chrono::Utc::now();
    let release_result =
        LeaderLeaseManager::release(&mut db, &lease_config.owner_label, release_at)
            .context("release leader lease after dispatch loop")?;

    match dispatch_result {
        Ok(outcome) => {
            let _ = db.write_event_log(
                grove_types::EventKind::CoordinatorStopped,
                None,
                None,
                None,
                &serde_json::json!({
                    "exit_reason": outcome.exit_reason.to_string(),
                    "stop_reason": outcome.stop_reason.as_str(),
                    "forced_termination": outcome.stop_reason == grove_types::CoordinatorStopReason::Interrupted,
                    "running_session_count": 0,
                    "leader_released": release_result.is_some(),
                }),
                &chrono::Utc::now(),
            );
            if json_mode {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "ok": true,
                        "command": "run",
                        "workspace_root": loaded.paths.workspace_root().as_str(),
                        "db_path": loaded.paths.db_path().as_str(),
                        "exit_reason": outcome.exit_reason.to_string(),
                        "stop_reason": outcome.stop_reason.as_str(),
                        "dispatched_count": outcome.dispatched_count,
                        "poll_cycles": outcome.poll_cycles,
                        "leader_lease_released": release_result.is_some(),
                        "configured_max_parallel": loaded.config.scheduler.max_parallel,
                    }))?
                );
            } else {
                println!("Dispatch loop exited: {}", outcome.exit_reason);
                println!("Stop reason: {}", outcome.stop_reason);
                println!("Total runs dispatched: {}", outcome.dispatched_count);
                println!("Total poll cycles: {}", outcome.poll_cycles);
                print_run_startup_report(&loaded, &release_result);
            }
            Ok(())
        }
        Err(error) => {
            let error_message = error.to_string();
            let _ = db.write_event_log(
                grove_types::EventKind::CoordinatorStopped,
                None,
                None,
                None,
                &serde_json::json!({
                    "exit_reason": "error",
                    "stop_reason": grove_types::CoordinatorStopReason::InternalError.as_str(),
                    "forced_termination": false,
                    "running_session_count": 0,
                    "leader_released": release_result.is_some(),
                    "error": error_message,
                }),
                &chrono::Utc::now(),
            );
            if json_mode {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "ok": false,
                        "command": "run",
                        "workspace_root": loaded.paths.workspace_root().as_str(),
                        "db_path": loaded.paths.db_path().as_str(),
                        "exit_reason": "error",
                        "stop_reason": grove_types::CoordinatorStopReason::InternalError.as_str(),
                        "leader_lease_released": release_result.is_some(),
                        "configured_max_parallel": loaded.config.scheduler.max_parallel,
                        "error": error_message,
                    }))?
                );
                Ok(())
            } else {
                print_run_startup_report(&loaded, &release_result);
                Err(error)
            }
        }
    }
}

fn handle_log(bead_id: &BeadId, json_mode: bool) -> Result<()> {
    let (_loaded, db, _br) = open_runtime()?;

    let runs = db
        .list_task_runs_for_bead(bead_id)
        .with_context(|| format!("list runs for {}", bead_id.as_str()))?;

    if runs.is_empty() {
        if json_mode {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "bead_id": bead_id.as_str(),
                    "run": null,
                    "events": [],
                    "latest_session": null,
                    "latest_checkpoint": null,
                    "recovery_capsule": null,
                    "transcript_tail": null,
                }))?
            );
        } else {
            println!("No runs found for bead {}.", bead_id.as_str());
        }
        return Ok(());
    }

    let latest_run = &runs[0];
    let events = db
        .list_events_for_run(&latest_run.id)
        .with_context(|| format!("list events for run {}", latest_run.id.as_str()))?;
    let latest_session = db.latest_session_for_run(&latest_run.id)?;
    let latest_checkpoint = db.latest_checkpoint_for_bead(bead_id)?;
    let recovery_capsule = db.latest_recovery_capsule_for_bead(bead_id)?;
    let transcript_tail = latest_session
        .as_ref()
        .map(|session| read_transcript_tail(&session.transcript_path, 20))
        .transpose()?;

    if json_mode {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "bead_id": bead_id.as_str(),
                "run": latest_run,
                "events": events,
                "latest_session": latest_session,
                "latest_checkpoint": latest_checkpoint,
                "recovery_capsule": recovery_capsule,
                "transcript_tail": transcript_tail,
            }))?
        );
        return Ok(());
    }

    println!("Bead: {}", bead_id.as_str());
    println!(
        "Run: {} (attempt {}, status: {:?})",
        latest_run.id.as_str(),
        latest_run.attempt_no,
        latest_run.status
    );
    println!("Started: {}", latest_run.started_at);
    println!("Ended: {}", display_option(latest_run.ended_at.as_ref()));
    if let Some(failure_class) = latest_run.failure_class {
        println!("Failure class: {:?}", failure_class);
    }
    if let Some(detail) = latest_run.failure_detail.as_deref() {
        println!("Failure detail: {detail}");
    }
    println!(
        "Sessions: {} | Checkpoints: {}",
        latest_run.session_count, latest_run.checkpoint_count
    );

    if events.is_empty() {
        println!("\nNo events recorded for this run.");
    } else {
        println!("\nEvent log ({} events):", events.len());
        for event in &events {
            let session_label = event
                .session_id
                .as_ref()
                .map(|sid| format!(" ses:{}", sid.as_str()))
                .unwrap_or_default();
            println!("  [{:?}]{} at {}", event.kind, session_label, event.created_at);
            if event.payload != serde_json::Value::Null {
                if let Ok(pretty) = serde_json::to_string(&event.payload) {
                    println!("    {pretty}");
                }
            }
        }
    }

    if let Some(session) = latest_session.as_ref() {
        println!("\nLatest session: {}", session.id.as_str());
        println!("  status: {:?}", session.status);
        println!("  transcript: {}", session.transcript_path);
        println!(
            "  stop reason: {}",
            display_option(session.stop_reason.as_ref().map(|reason| format!("{reason:?}")))
        );
        match transcript_tail.as_ref() {
            Some(Some(lines)) if !lines.is_empty() => {
                println!("\nTranscript tail:");
                for line in lines {
                    println!("  {line}");
                }
            }
            Some(Some(_)) => println!("  (transcript file is empty)"),
            Some(None) => println!("  (transcript file not found at {})", session.transcript_path),
            None => println!("  (transcript unavailable)"),
        }
    }

    if let Some(checkpoint) = latest_checkpoint.as_ref() {
        println!("\nLatest checkpoint:");
        println!("  progress: {}", checkpoint.progress);
        println!("  next step: {}", checkpoint.next_step);
        println!("  saved at: {}", checkpoint.saved_at);
        println!("  resume generation: {}", checkpoint.resume_generation);
    }

    if let Some(capsule_event) = recovery_capsule.as_ref() {
        println!("\nRecovery capsule:");
        println!("  outcome: {:?}", capsule_event.capsule.outcome);
        println!("  summary: {}", capsule_event.capsule.summary);
        if !capsule_event.capsule.likely_root_causes.is_empty() {
            println!(
                "  root causes: {}",
                capsule_event.capsule.likely_root_causes.join(" | ")
            );
        }
    }

    Ok(())
}

fn handle_retry(bead_id: &BeadId, json_mode: bool) -> Result<()> {
    let (_loaded, mut db, br) = open_runtime()?;

    // Check current bead status.
    let bead = db
        .get_bead_record(bead_id)
        .with_context(|| format!("load bead record for {}", bead_id.as_str()))?
        .ok_or_else(|| anyhow!("bead {} not found in local cache", bead_id.as_str()))?;

    // Only allow retry for Failed or Checkpointed beads.
    match bead.grove_status {
        GroveBeadStatus::Failed | GroveBeadStatus::Checkpointed => {}
        other => {
            bail!(
                "bead {} has grove status {:?}, can only retry Failed or Checkpointed beads",
                bead_id.as_str(),
                other
            );
        }
    }

    // Check br still reports this bead as ready (or at least open).
    let br_ready = br.ready().unwrap_or_default();
    let is_ready_in_br = br_ready.iter().any(|s| &s.id == bead_id);

    if json_mode {
        let warning = (!is_ready_in_br).then(|| {
            format!(
                "bead {} is not reported as ready by br; proceeding with local retry",
                bead_id.as_str()
            )
        });
        let now = chrono::Utc::now();
        db.reset_bead_for_retry(bead_id, &now)
            .with_context(|| format!("reset bead {} for retry", bead_id.as_str()))?;
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "bead_id": bead_id.as_str(),
                "reset": true,
                "previous_grove_status": format!("{:?}", bead.grove_status),
                "previous_failure_class": bead.last_failure_class.map(|class| format!("{:?}", class)),
                "ready_in_br": is_ready_in_br,
                "warning": warning,
                "next_action": "run grove run to dispatch the bead again",
            }))?
        );
        return Ok(());
    }

    if !is_ready_in_br {
        println!(
            "Warning: bead {} is not reported as ready by br. Proceeding with local retry anyway.",
            bead_id.as_str()
        );
    }

    let now = chrono::Utc::now();
    db.reset_bead_for_retry(bead_id, &now)
        .with_context(|| format!("reset bead {} for retry", bead_id.as_str()))?;

    println!("Bead {} reset to Ready for retry.", bead_id.as_str());
    println!("Previous status: {:?}", bead.grove_status);
    if let Some(failure_class) = bead.last_failure_class {
        println!("Previous failure: {:?}", failure_class);
    }
    println!("\nThe bead will be dispatched in the next `grove run` cycle.");

    Ok(())
}

fn run_startup_checks(
    db: &mut Database,
    lease_config: &LeaderLeaseConfig,
    startup: grove_kernel::StartupCoordinatorState,
) -> Result<()> {
    LeaderLeaseManager::heartbeat(db, lease_config, chrono::Utc::now())?
        .ok_or_else(|| anyhow!("leader lease heartbeat failed after acquisition"))?;
    print_startup_recovery_report(&startup.leader, &startup.recovery);
    Ok(())
}

fn open_runtime() -> Result<(LoadedConfig, Database, CliBrClient)> {
    let workspace_root = current_workspace_root()?;
    let loaded = load_from_workspace(&workspace_root).with_context(|| {
        format!(
            "load grove configuration from {}",
            workspace_root.join("grove.toml")
        )
    })?;
    ensure_workspace_layout(&loaded.paths)?;

    let br = CliBrClient::new("br", loaded.paths.workspace_root().as_str());
    let br_capability = br.capability().context("check br capability")?;
    if !br_capability.beads_dir_exists {
        bail!(
            "{} does not contain a .beads directory; run `br init` first",
            loaded.paths.workspace_root()
        );
    }

    let mut db = Database::open(loaded.paths.db_path())
        .with_context(|| format!("open database at {}", loaded.paths.db_path()))?;
    db.migrate().context("apply database migrations")?;
    sync_bead_cache(&br, &mut db).context("sync bead cache from br")?;

    Ok((loaded, db, br))
}

fn current_workspace_root() -> Result<Utf8PathBuf> {
    let cwd = env::current_dir().context("read current working directory")?;
    for candidate in cwd.ancestors() {
        if candidate.join("grove.toml").is_file() {
            return Utf8PathBuf::from_path_buf(candidate.to_path_buf()).map_err(|path| {
                anyhow::anyhow!("working directory is not valid UTF-8: {}", path.display())
            });
        }
    }
    Utf8PathBuf::from_path_buf(cwd)
        .map_err(|path| anyhow::anyhow!("working directory is not valid UTF-8: {}", path.display()))
}

fn write_default_config(path: &Utf8PathBuf) -> Result<()> {
    let text =
        toml::to_string_pretty(&GroveConfig::default()).context("serialize default config")?;
    fs::write(path, format!("{text}\n"))
        .with_context(|| format!("write default config to {path}"))?;
    Ok(())
}

fn ensure_workspace_layout(paths: &GrovePaths) -> Result<()> {
    let mut dirs = vec![
        paths.grove_dir().to_owned(),
        paths.transcript_dir().to_owned(),
        paths.prompts_dir(),
        paths.checkpoints_dir(),
        paths.artifacts_dir(),
        paths.logs_dir(),
        paths.tmp_dir(),
    ];
    if let Some(parent) = paths.db_path().parent() {
        dirs.push(parent.to_owned());
    }

    for dir in dirs {
        fs::create_dir_all(&dir).with_context(|| format!("create managed directory {dir}"))?;
    }

    Ok(())
}

fn ensure_required_tooling(tooling: &RequiredTooling) -> Result<()> {
    for tool in [&tooling.claude, &tooling.br, &tooling.bv] {
        ensure_tool_available(tool)?;
    }
    Ok(())
}

fn ensure_tool_available(tool: &ToolCapability) -> Result<()> {
    if tool.available {
        Ok(())
    } else {
        bail!("required tool `{}` is not available on PATH", tool.binary)
    }
}

fn print_status_view(
    view: &WorkspaceStatusView,
    db_path: &str,
    triage: Option<&BvTriageOutput>,
    triage_error: Option<&str>,
) {
    println!("Workspace: {}", view.workspace_root);
    println!("Database: {db_path}");
    match &view.leader {
        Some(leader) => {
            println!("Leader: {}", leader.owner_label);
            if let Some(heartbeat_at) = leader.heartbeat_at {
                println!("Leader heartbeat: {heartbeat_at}");
            }
            if let Some(expires_at) = leader.expires_at {
                println!("Leader expires: {expires_at}");
            }
        }
        None => println!("Leader: none"),
    }
    match &view.last_coordinator_stop {
        Some(stop) => {
            println!("Last coordinator stop: {} at {}", stop.reason, stop.created_at);
            println!("Coordinator forced termination: {}", if stop.forced { "yes" } else { "no" });
            println!(
                "Coordinator leader released: {}",
                display_option(stop.leader_released)
            );
            println!(
                "Coordinator running sessions at stop: {}",
                display_option(stop.running_session_count)
            );
        }
        None => println!("Last coordinator stop: none"),
    }

    println!("\nBeads status counts:");
    if view.bead_status_counts.is_empty() {
        println!("- none");
    } else {
        for count in &view.bead_status_counts {
            println!("- {}: {}", count.status, count.count);
        }
    }

    println!("\nGrove runtime counts:");
    if view.grove_status_counts.is_empty() {
        println!("- none");
    } else {
        for count in &view.grove_status_counts {
            println!("- {}: {}", count.status, count.count);
        }
    }

    println!("\nRunning beads:");
    if view.running_beads.is_empty() {
        println!("- none");
    } else {
        for bead in &view.running_beads {
            println!(
                "- {} [{}] {}",
                bead.bead_id,
                format_priority(bead.priority),
                bead.title
            );
            println!("  run: {}", display_option(bead.run_id.as_ref()));
            println!("  session: {}", display_option(bead.session_id.as_ref()));
            println!("  started: {}", display_option(bead.started_at.as_ref()));
            if let Some(context_pressure_pct) = bead.context_pressure_pct {
                println!("  context pressure: {:.1}%", context_pressure_pct * 100.0);
            }
            if let Some(last_progress) = bead.last_progress.as_deref() {
                println!("  progress: {last_progress}");
            }
        }
    }

    println!("\nReady queue:");
    if view.ready_queue.is_empty() {
        println!("- none");
    } else {
        for entry in &view.ready_queue {
            println!(
                "- {} [{}] score {} — {}",
                entry.bead_id,
                format_priority(entry.priority),
                format_score(entry.score),
                entry.dispatch.summary()
            );
            if !entry.why.is_empty() {
                println!("  why: {}", entry.why.join(", "));
            }
            if !entry.score_breakdown.is_empty() {
                let breakdown = entry
                    .score_breakdown
                    .iter()
                    .map(|component| format!("{}={:.1}", component.label, component.value))
                    .collect::<Vec<_>>()
                    .join(", ");
                println!("  score breakdown: {breakdown}");
            }
            if !entry.dispatch.local_suppression_reasons.is_empty() {
                let suppressions = entry
                    .dispatch
                    .local_suppression_reasons
                    .iter()
                    .map(|reason| reason.summary.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                println!("  suppressions: {suppressions}");
            }
            if entry.mirror_pending {
                println!("  mirror pending: yes");
            }
        }
    }

    println!("\nCheckpointed beads:");
    if view.checkpointed_beads.is_empty() {
        println!("- none");
    } else {
        for bead in &view.checkpointed_beads {
            println!("- {} {}", bead.bead_id, bead.title);
            println!("  run: {}", display_option(bead.run_id.as_ref()));
            println!(
                "  checkpoint: {}",
                display_option(bead.checkpoint_id.as_deref())
            );
            println!("  saved: {}", display_option(bead.saved_at.as_ref()));
            if let Some(progress) = bead.progress.as_deref() {
                println!("  progress: {progress}");
            }
            if let Some(next_step) = bead.next_step.as_deref() {
                println!("  next step: {next_step}");
            }
            if !bead.claimed_paths.is_empty() {
                println!("  claimed paths: {}", bead.claimed_paths.join(", "));
            }
        }
    }

    println!("\nFailed beads:");
    if view.failed_beads.is_empty() {
        println!("- none");
    } else {
        for bead in &view.failed_beads {
            println!(
                "- {} [{}] {}",
                bead.bead_id,
                format_priority(bead.priority),
                bead.title
            );
            println!("  run: {}", display_option(bead.run_id.as_ref()));
            println!(
                "  failure class: {}",
                display_option(
                    bead.failure_class
                        .as_ref()
                        .map(|class| format!("{class:?}"))
                )
            );
            println!(
                "  retry after: {}",
                display_option(bead.retry_after.as_ref())
            );
            if let Some(detail) = bead.failure_detail.as_deref() {
                println!("  detail: {detail}");
            }
            if let Some(dispatch) = bead.dispatch.as_ref() {
                println!("  dispatch: {}", dispatch.summary());
            }
            if let Some(recovery_hint) = bead.recovery_hint.as_deref() {
                println!("  recovery: {recovery_hint}");
            }
            if let Some(capsule) = bead.recovery_capsule.as_ref() {
                println!("  capsule: {}", capsule.compact_summary());
            }
            if bead.mirror_pending {
                println!("  mirror pending: yes");
            }
        }
    }

    println!("\nReservation conflicts:");
    if view.reservation_conflicts.is_empty() {
        println!("- none");
    } else {
        for conflict in &view.reservation_conflicts {
            println!(
                "- {} ({}) overlaps {} ({})",
                conflict.requested_by_bead,
                conflict.requested_pattern,
                conflict.conflicting_bead,
                conflict.held_pattern
            );
            println!(
                "  conflicting run: {}",
                display_option(conflict.conflicting_run_id.as_ref())
            );
        }
    }

    println!("\nMirror pending:");
    if view.mirror_pending.is_empty() {
        println!("- none");
    } else {
        for pending in &view.mirror_pending {
            println!("- {}", pending.bead_id);
            println!("  run: {}", display_option(pending.run_id.as_ref()));
            println!("  pending actions: {}", pending.pending_actions.join(", "));
            println!(
                "  last attempt: {}",
                display_option(pending.last_attempt_at.as_ref())
            );
            if let Some(last_error) = pending.last_error.as_deref() {
                println!("  last error: {last_error}");
            }
        }
    }

    match triage {
        Some(triage) => {
            println!("\nBV triage:");
            println!("- actionable: {}", triage.quick_ref.actionable_count);
            println!("- blocked: {}", triage.quick_ref.blocked_count);
            println!("- in progress: {}", triage.quick_ref.in_progress_count);
            if let Some(top_pick) = triage.quick_ref.top_picks.first() {
                println!("- top pick: {} — {}", top_pick.id, top_pick.title);
            } else if let Some(recommendation) = triage.recommendations.first() {
                println!(
                    "- top recommendation: {} — {}",
                    recommendation.id, recommendation.title
                );
            }
        }
        None => {
            if let Some(error) = triage_error {
                println!("\nBV triage:");
                println!("- unavailable: {error}");
            }
        }
    }
}

fn print_inspect_report(
    workspace_root: &str,
    issue_detail: Option<&BrIssueDetail>,
    view: Option<&BeadInspectView>,
) {
    println!("Workspace: {workspace_root}");

    if let Some(detail) = issue_detail {
        println!("Bead: {} — {}", detail.summary.id, detail.summary.title);
        println!("Priority: {}", format_priority(detail.summary.priority));
        println!("Type: {}", detail.summary.issue_type);
        println!("Beads status: {}", detail.summary.status);
        println!(
            "Assignee: {}",
            display_option(detail.summary.assignee.as_deref())
        );
        if !detail.summary.labels.is_empty() {
            println!("Labels: {}", detail.summary.labels.join(", "));
        }
        if let Some(closed_at) = detail.closed_at {
            println!("Closed at: {closed_at}");
        }
        if let Some(close_reason) = detail.close_reason.as_deref() {
            println!("Close reason: {close_reason}");
        }
        if let Some(description) = detail.summary.description.as_deref() {
            println!("\nDescription:\n{description}");
        }
        if !detail.comments.is_empty() {
            println!("\nComments: {}", detail.comments.len());
        }
    } else if let Some(view) = view {
        println!("Bead: {} — {}", view.bead.bead.id, view.bead.bead.title);
        println!("Priority: {}", format_priority(view.bead.bead.priority));
        println!("Type: {}", view.bead.bead.issue_type);
        println!("Beads status: {}", view.bead.bead.br_status);
        if !view.bead.bead.labels.is_empty() {
            println!("Labels: {}", view.bead.bead.labels.join(", "));
        }
    }

    match view {
        Some(view) => {
            println!("\nGrove runtime:");
            println!("- status: {:?}", view.bead.grove_status);
            println!("- synced at: {}", view.bead.synced_at);
            println!("- runtime updated: {}", view.bead.runtime_updated_at);
            println!(
                "- last run: {}",
                display_option(view.bead.last_run_id.as_ref())
            );
            println!(
                "- retry after: {}",
                display_option(view.bead.retry_after.as_ref())
            );
            if !view.bead.declared_paths.is_empty() {
                println!("- declared paths: {}", view.bead.declared_paths.join(", "));
            }

            println!("\nDependencies:");
            if view.dependencies.is_empty() {
                println!("- none");
            } else {
                for dependency in &view.dependencies {
                    println!(
                        "- {} — {} [br: {}, grove: {}]",
                        dependency.bead_id,
                        dependency.title.as_deref().unwrap_or("-"),
                        dependency.br_status.as_deref().unwrap_or("-"),
                        dependency.grove_status.as_deref().unwrap_or("-")
                    );
                }
            }

            println!("\nDependents:");
            if view.dependents.is_empty() {
                println!("- none");
            } else {
                for dependent in &view.dependents {
                    println!(
                        "- {} — {} [br: {}, grove: {}]",
                        dependent.bead_id,
                        dependent.title.as_deref().unwrap_or("-"),
                        dependent.br_status.as_deref().unwrap_or("-"),
                        dependent.grove_status.as_deref().unwrap_or("-")
                    );
                }
            }

            println!("\nLatest dispatch:");
            match view.latest_dispatch.as_ref() {
                Some(dispatch) => {
                    println!(
                        "- attempted at: {}",
                        display_option(dispatch.attempted_at.as_ref())
                    );
                    println!("- summary: {}", dispatch.dispatch.summary());
                    println!("- score: {}", format_score(dispatch.score));
                    if let Some(bv_score) = dispatch.bv_score {
                        println!("- bv score: {:.2}", bv_score);
                    }
                    if let Some(ready_minutes) = dispatch.ready_minutes {
                        println!("- ready age: {} minute(s)", ready_minutes);
                    }
                    if !dispatch.score_breakdown.is_empty() {
                        let breakdown = dispatch
                            .score_breakdown
                            .iter()
                            .map(|component| format!("{}={:.1}", component.label, component.value))
                            .collect::<Vec<_>>()
                            .join(", ");
                        println!("- score breakdown: {breakdown}");
                    }
                    if !dispatch.why.is_empty() {
                        println!("- why: {}", dispatch.why.join(", "));
                    }
                    if !dispatch.dispatch.local_suppression_reasons.is_empty() {
                        println!("- suppressions:");
                        for reason in &dispatch.dispatch.local_suppression_reasons {
                            println!("  - {}", reason.summary);
                            if let Some(conflict) = reason.conflict.as_ref() {
                                println!(
                                    "    overlaps {} ({}) ; conflicting run: {}",
                                    conflict.conflicting_bead,
                                    conflict.held_pattern,
                                    display_option(conflict.conflicting_run_id.as_ref())
                                );
                            }
                        }
                    }
                    if !dispatch.reservation_conflicts.is_empty() {
                        println!("- reservation conflicts:");
                        for conflict in &dispatch.reservation_conflicts {
                            println!(
                                "  - {} ({}) overlaps {} ({})",
                                conflict.requested_by_bead,
                                conflict.requested_pattern,
                                conflict.conflicting_bead,
                                conflict.held_pattern
                            );
                            println!(
                                "    conflicting run: {}",
                                display_option(conflict.conflicting_run_id.as_ref())
                            );
                        }
                    }
                }
                None => println!("- none"),
            }

            println!("\nRun history:");
            if view.run_history.is_empty() {
                println!("- none");
            } else {
                for run in &view.run_history {
                    println!(
                        "- {} attempt {} [{}] started {}",
                        run.run_id, run.attempt_no, run.status, run.started_at
                    );
                    println!("  ended: {}", display_option(run.ended_at.as_ref()));
                    println!(
                        "  failure class: {}",
                        display_option(run.failure_class.as_deref())
                    );
                    println!(
                        "  failure detail: {}",
                        display_option(run.failure_detail.as_deref())
                    );
                    println!(
                        "  sessions/checkpoints: {}/{}",
                        run.session_count, run.checkpoint_count
                    );
                }
            }

            println!("\nLatest session:");
            match view.latest_session.as_ref() {
                Some(session) => {
                    println!("- session: {}", session.session_id);
                    println!("- run: {}", session.run_id);
                    println!("- status: {}", session.status);
                    println!("- started: {}", session.started_at);
                    println!("- ended: {}", display_option(session.ended_at.as_ref()));
                    println!(
                        "- stop reason: {}",
                        display_option(session.stop_reason.as_deref())
                    );
                    println!(
                        "- terminal class: {}",
                        display_option(session.terminal_class.as_deref())
                    );
                    println!("- exit code: {}", display_option(session.exit_code));
                    println!("- transcript: {}", session.transcript_path);
                    println!(
                        "- prompt id: {}",
                        display_option(session.prompt_id.as_deref())
                    );
                    println!(
                        "- prompt manifest: {}",
                        display_option(session.prompt_manifest_path.as_deref())
                    );
                    if let Some(prompt) = session.prompt_provenance.as_ref() {
                        println!("- prompt contract: {}", prompt.contract);
                        println!("- prompt estimated tokens: {}", prompt.estimated_tokens);
                        println!("- prompt bytes: {}", prompt.prompt_bytes);
                        println!("- prompt trimmed: {}", prompt.trimmed);
                        println!(
                            "- retry delta summary: {}",
                            display_option(prompt.retry_delta_summary.as_deref())
                        );
                        if prompt.sections.is_empty() {
                            println!("- prompt sections: none");
                        } else {
                            println!("- prompt sections:");
                            for section in &prompt.sections {
                                println!(
                                    "  - #{} {} [{}] included={} tokens={} trim={} ",
                                    section.ordinal,
                                    section.heading,
                                    section.kind,
                                    section.included,
                                    section.estimated_tokens,
                                    display_option(section.trim_reason.as_deref())
                                );
                                println!("    preview: {}", section.preview);
                            }
                        }
                    }
                    println!(
                        "- result summary: {}",
                        display_option(session.result_summary.as_deref())
                    );
                    println!(
                        "- completion indicators: {}",
                        display_option(session.completion_indicators)
                    );
                    println!("- explicit exit: {}", display_option(session.explicit_exit));
                }
                None => println!("- none"),
            }

            println!("\nLatest checkpoint:");
            match view.latest_checkpoint.as_ref() {
                Some(checkpoint) => {
                    println!("- checkpoint: {}", checkpoint.checkpoint_id);
                    println!("- run: {}", checkpoint.run_id);
                    println!("- session: {}", checkpoint.session_id);
                    println!("- progress: {}", checkpoint.progress);
                    println!("- next step: {}", checkpoint.next_step);
                    println!("- saved at: {}", checkpoint.saved_at);
                    println!("- resume generation: {}", checkpoint.resume_generation);
                }
                None => println!("- none"),
            }

            println!("\nRecovery capsule:");
            match view.latest_recovery_capsule.as_ref() {
                Some(capsule) => {
                    println!("- outcome: {}", capsule.outcome);
                    println!("- summary: {}", capsule.summary);
                    println!(
                        "- next attempt contract: {}",
                        display_option(capsule.next_attempt_contract.as_deref())
                    );
                    println!(
                        "- retry delta summary: {}",
                        display_option(capsule.retry_delta_summary.as_deref())
                    );
                    println!(
                        "- checkpoint progress: {}",
                        display_option(capsule.checkpoint_progress.as_deref())
                    );
                    println!(
                        "- checkpoint next step: {}",
                        display_option(capsule.checkpoint_next_step.as_deref())
                    );
                    if !capsule.strongest_evidence.is_empty() {
                        println!(
                            "- strongest evidence: {}",
                            capsule.strongest_evidence.join(" | ")
                        );
                    }
                    if !capsule.likely_root_causes.is_empty() {
                        println!(
                            "- likely root causes: {}",
                            capsule.likely_root_causes.join(" | ")
                        );
                    }
                    if !capsule.risky_paths.is_empty() {
                        println!("- risky paths: {}", capsule.risky_paths.join(" | "));
                    }
                    if !capsule.do_not_repeat.is_empty() {
                        println!("- do not repeat: {}", capsule.do_not_repeat.join(" | "));
                    }
                    if !capsule.artifacts.is_empty() {
                        println!("- artifacts: {}", capsule.artifacts.join(", "));
                    }
                }
                None => println!("- none"),
            }

            println!("\nLatest handoff:");
            match view.latest_handoff.as_ref() {
                Some(handoff) => {
                    println!("- run: {}", handoff.run_id);
                    println!("- completed at: {}", handoff.completed_at);
                    println!("- summary: {}", handoff.summary);
                    if !handoff.artifacts.is_empty() {
                        println!("- artifacts: {}", handoff.artifacts.join(", "));
                    }
                    if !handoff.lessons.is_empty() {
                        println!("- lessons: {}", handoff.lessons.join(" | "));
                    }
                    if !handoff.decisions.is_empty() {
                        println!("- decisions: {}", handoff.decisions.join(" | "));
                    }
                    if !handoff.warnings.is_empty() {
                        println!("- warnings: {}", handoff.warnings.join(" | "));
                    }
                }
                None => println!("- none"),
            }

            println!("\nMirror actions:");
            if view.mirror_actions.is_empty() {
                println!("- none");
            } else {
                for action in &view.mirror_actions {
                    println!("- {} at {}", action.action, action.created_at);
                    println!("  succeeded: {}", display_option(action.succeeded));
                    println!("  detail: {}", display_option(action.detail.as_deref()));
                }
            }

            println!("\nRetrieval summary:");
            match view.retrieval_summary.as_ref() {
                Some(summary) => {
                    println!("- conversations: {}", summary.conversation_ids.len());
                    println!("- snippets: {}", summary.snippet_count);
                    for snippet in &summary.top_snippets {
                        println!(
                            "  - conversation {} message {} score {:.2}",
                            snippet.conversation_id, snippet.message_id, snippet.score
                        );
                        println!("    file: {}", display_option(snippet.file_path.as_deref()));
                        println!("    snippet: {}", snippet.snippet);
                    }
                }
                None => println!("- none"),
            }

            println!("\nPlaybook bullets:");
            if view.playbook_bullets.is_empty() {
                println!("- none");
            } else {
                for bullet in &view.playbook_bullets {
                    println!(
                        "- {} [{} / {}] {}",
                        bullet.bullet_id, bullet.category, bullet.maturity, bullet.text
                    );
                }
            }

            println!("\nMirror pending:");
            match view.mirror_pending.as_ref() {
                Some(pending) => {
                    println!("- bead: {}", pending.bead_id);
                    println!("- run: {}", display_option(pending.run_id.as_ref()));
                    println!("- pending actions: {}", pending.pending_actions.join(", "));
                    println!(
                        "- last attempt: {}",
                        display_option(pending.last_attempt_at.as_ref())
                    );
                    println!(
                        "- last error: {}",
                        display_option(pending.last_error.as_deref())
                    );
                }
                None => println!("- none"),
            }
        }
        None => {
            println!("\nNo local Grove runtime record is available for this bead yet.");
        }
    }
}

fn print_startup_recovery_report(leader: &LeaderLeaseRecord, recovery: &StartupRecoveryReport) {
    println!("Leader lease acquired.");
    println!("- owner: {}", leader.owner_label);
    println!("- acquired at: {}", leader.acquired_at);
    println!("- heartbeat at: {}", leader.heartbeat_at);
    println!("- expires at: {}", leader.expires_at);

    println!("\nStartup reconciliation:");
    println!("- interrupted runs: {}", recovery.interrupted_runs.len());
    for interrupted in &recovery.interrupted_runs {
        println!(
            "  - {} run {} -> {:?}",
            interrupted.bead_id, interrupted.run.id, interrupted.run.status
        );
    }
    println!(
        "- recovered reservations: {}",
        recovery.reservations.recovered.len()
    );
    for recovered in &recovery.reservations.recovered {
        println!(
            "  - {} {}",
            recovered.reservation.bead_id, recovered.reservation.path_pattern
        );
    }
    println!(
        "- expired reservations: {}",
        recovery.reservations.expired.len()
    );
    for expired in &recovery.reservations.expired {
        println!("  - {} {}", expired.bead_id, expired.path_pattern);
    }
}

fn print_run_startup_report(loaded: &LoadedConfig, released_lease: &Option<LeaderLeaseRecord>) {
    println!("\nRun startup summary:");
    println!("- workspace: {}", loaded.paths.workspace_root());
    println!("- db: {}", loaded.paths.db_path());
    println!(
        "- configured max_parallel: {}",
        loaded.config.scheduler.max_parallel
    );
    println!(
        "- leader lease released: {}",
        if released_lease.is_some() {
            "yes"
        } else {
            "no"
        }
    );
}

fn render_tool_line(binary: &str, version: Option<&str>) -> String {
    match version {
        Some(version) => format!("{binary} ({version})"),
        None => binary.to_owned(),
    }
}

fn format_priority(priority: BeadPriority) -> &'static str {
    match priority {
        BeadPriority::P0 => "P0",
        BeadPriority::P1 => "P1",
        BeadPriority::P2 => "P2",
        BeadPriority::P3 => "P3",
        BeadPriority::P4 => "P4",
    }
}

fn format_score(score: Option<f64>) -> String {
    match score {
        Some(score) => format!("{score:.1}"),
        None => "-".to_owned(),
    }
}

fn display_option<T>(value: Option<T>) -> String
where
    T: std::fmt::Display,
{
    match value {
        Some(value) => value.to_string(),
        None => "-".to_owned(),
    }
}

fn read_transcript_tail(path: &str, tail_lines: usize) -> Result<Option<Vec<String>>> {
    let transcript_path = std::path::Path::new(path);
    if !transcript_path.exists() {
        return Ok(None);
    }

    let content = fs::read_to_string(transcript_path)
        .with_context(|| format!("read transcript at {path}"))?;
    let lines = content.lines().map(ToOwned::to_owned).collect::<Vec<_>>();
    let start = lines.len().saturating_sub(tail_lines);
    Ok(Some(lines[start..].to_vec()))
}

fn command_name(command: &Command) -> &'static str {
    match command {
        Command::Init => "init",
        Command::Status => "status",
        Command::Inspect { .. } => "inspect",
        Command::Log { .. } => "log",
        Command::Retry { .. } => "retry",
        Command::Run => "run",
    }
}
