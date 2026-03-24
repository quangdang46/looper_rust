use anyhow::{Context, Result, anyhow, bail};
use camino::{Utf8Path, Utf8PathBuf};
use clap::{ArgAction, Parser, Subcommand};
use crossterm::{
    event::{self, Event as CEvent, KeyCode},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use grove_br::{BrClient, BrIssueDetail, CliBrClient, sync_bead_cache};
use grove_bv::{BvClient, BvTriageOutput, CliBvClient};
use grove_config::{
    DEFAULT_INIT_GROVE_TOML, GrovePaths, LoadedConfig, RequiredTooling, ToolCapability,
    detect_required_tooling, load_from_workspace,
};
use grove_db::Database;
use grove_kernel::{
    BeadInspectView, DispatchExitReason, DispatchLoopConfig, DispatchLoopOutcome,
    LeaderLeaseConfig, LeaderLeaseManager, ShutdownSignal, StartupRecoveryReport,
    WorkspaceStatusView, acquire_startup_coordinator, init_trace_logging,
    load_bead_inspect_view, load_workspace_status_view, run_dispatch_loop, trace_runtime_event,
};
use grove_session::replay_transcript;
use grove_types::{
    AgentActivity, BeadId, BeadPriority, EventKind, GroveBeadRecord, GroveBeadStatus,
    LeaderLeaseRecord, ProtocolEvent, RunReport, RunStatus, TranscriptEvent,
};
use rusqlite::OptionalExtension;
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Tabs, Wrap},
};
use serde_json::json;
use std::{cmp, env, fs, io, io::IsTerminal, sync::mpsc, thread, time::Duration};

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
    Init {
        #[arg(long, action = ArgAction::SetTrue)]
        force: bool,
    },
    Status,
    Inspect { bead_id: String },
    Log { bead_id: String },
    Retry { bead_id: String },
    Run {
        #[arg(long, action = ArgAction::SetTrue)]
        live: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Init { force }) => {
            run_json_command("init", cli.json, || handle_init(cli.json, force))
        }
        Some(Command::Status) => handle_status(cli.json),
        Some(Command::Inspect { bead_id }) => handle_inspect(&BeadId::new(bead_id), cli.json),
        Some(Command::Log { bead_id }) => handle_log(&BeadId::new(bead_id), cli.json),
        Some(Command::Retry { bead_id }) => handle_retry(&BeadId::new(bead_id), cli.json),
        Some(Command::Run { live }) => run_json_command("run", cli.json, || handle_run(cli.json, live)),
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

fn handle_init(json_mode: bool, force: bool) -> Result<()> {
    let workspace_root = current_workspace_root()?;
    let config_path = workspace_root.join("grove.toml");
    let paths = resolve_init_paths(&workspace_root, &config_path)?;
    let existing_artifacts = existing_init_artifacts(&paths);

    if !existing_artifacts.is_empty() {
        if !force {
            bail!(
                "Grove is already initialized in {}.\n\nNothing was changed.\nIf you want a clean local Grove state, run:\n  grove init --force\n\nThis resets Grove-managed runtime state only and keeps grove.toml.",
                workspace_root,
            );
        }
        reset_managed_init_state(&paths)?;
    }

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
            force.then_some("Forced reset requested; Grove-managed runtime state was cleared before initialization."),
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
                "forced_reset": force,
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

    if force {
        println!("Reset existing Grove-managed state before initialization.");
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

    if !br_capability.beads_dir_exists || !bv_capability.beads_dir_exists || force {
        println!("\nNotes:");
        if force {
            println!("- Forced reset requested; Grove-managed runtime state was cleared before initialization.");
        }
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

fn handle_run(json_mode: bool, live: bool) -> Result<()> {
    if json_mode && live {
        bail!("`grove run --live` does not support --json");
    }

    let (loaded, mut db, br) = open_runtime()?;
    init_trace_logging(loaded.paths.workspace_root(), loaded.config.logging.persist_jsonl)?;
    trace_runtime_event(
        "coordinator.run_started",
        serde_json::json!({
            "workspace_root": loaded.paths.workspace_root().as_str(),
            "live": live,
            "json_mode": json_mode,
            "configured_max_parallel": loaded.config.scheduler.max_parallel,
            "persist_jsonl": loaded.config.logging.persist_jsonl,
        }),
    );
    let owner_label = format!("{}:{}", loaded.paths.workspace_root(), std::process::id());
    // Keep the leader lease comfortably above a single scheduler cycle so
    // normal DB / scoring / br work cannot self-expire the coordinator.
    let lease_ttl = chrono::Duration::seconds(30).max(chrono::Duration::milliseconds(
        cmp::max(1, loaded.config.scheduler.poll_interval_ms as i64) * 5,
    ));
    let lease_config = LeaderLeaseConfig {
        owner_label,
        lease_ttl,
    };
    let now = chrono::Utc::now();
    let startup = acquire_startup_coordinator(&mut db, &lease_config, None, now)
        .map_err(|error| anyhow!(error.to_string()))?;

    trace_runtime_event(
        "coordinator.lease_acquired",
        serde_json::json!({
            "owner_label": lease_config.owner_label.clone(),
            "lease_ttl_ms": lease_config.lease_ttl.num_milliseconds(),
        }),
    );

    run_startup_checks(&mut db, &lease_config, startup)?;
    if !json_mode && !live {
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

    let dispatch_result = if live {
        run_dispatch_loop_with_live_ui(&loaded, &br, &lease_config, &loop_config)?
    } else {
        run_dispatch_loop(
            &mut db,
            &backend,
            &br,
            &loaded.config,
            &lease_config,
            &loop_config,
        )
    };

    let release_at = chrono::Utc::now();
    let release_result =
        LeaderLeaseManager::release(&mut db, &lease_config.owner_label, release_at)
            .context("release leader lease after dispatch loop")?;
    trace_runtime_event(
        "coordinator.lease_released",
        serde_json::json!({
            "owner_label": lease_config.owner_label.clone(),
            "released": release_result.is_some(),
        }),
    );

    match dispatch_result {
        Ok(outcome) => {
            trace_runtime_event(
                "coordinator.run_stopped",
                serde_json::json!({
                    "exit_reason": outcome.exit_reason.to_string(),
                    "stop_reason": outcome.stop_reason.as_str(),
                    "dispatched_count": outcome.dispatched_count,
                    "poll_cycles": outcome.poll_cycles,
                }),
            );
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
                if outcome.exit_reason == DispatchExitReason::QueueEmpty {
                    println!("No runnable beads remain right now. The project may still have unfinished work that is blocked locally; check `grove status` for active runs, checkpoints, retry backoff, failed-awaiting-manual-retry, reservation conflicts, or `dispatch:no` suppressions.");
                }
                print_run_startup_report(&loaded, &release_result);
            }
            Ok(())
        }
        Err(error) => {
            let error_message = error.to_string();
            trace_runtime_event(
                "coordinator.run_error",
                serde_json::json!({
                    "error": error_message,
                    "leader_lease_released": release_result.is_some(),
                }),
            );
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
            println!(
                "  [{:?}]{} at {}",
                event.kind, session_label, event.created_at
            );
            if event.payload != serde_json::Value::Null
                && let Ok(pretty) = serde_json::to_string(&event.payload)
            {
                println!("    {pretty}");
            }
        }
    }

    if let Some(session) = latest_session.as_ref() {
        println!("\nLatest session: {}", session.id.as_str());
        println!("  status: {:?}", session.status);
        println!("  transcript: {}", session.transcript_path);
        println!(
            "  stop reason: {}",
            display_option(
                session
                    .stop_reason
                    .as_ref()
                    .map(|reason| format!("{reason:?}"))
            )
        );
        match transcript_tail.as_ref() {
            Some(Some(lines)) if !lines.is_empty() => {
                println!("\nTranscript tail:");
                for line in lines {
                    println!("  {line}");
                }
            }
            Some(Some(_)) => println!("  (transcript file is empty)"),
            Some(None) => println!(
                "  (transcript file not found at {})",
                session.transcript_path
            ),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LiveTab {
    Status,
    Live,
}

#[derive(Debug, Clone)]
struct LiveSessionSummary {
    bead_id: BeadId,
    title: String,
    run_id: Option<String>,
    session_id: Option<String>,
    transcript_path: Option<String>,
    started_at: Option<String>,
    run_status: Option<RunStatus>,
    activity: Option<AgentActivity>,
    failure_detail: Option<String>,
}

#[derive(Debug, Clone)]
struct LiveAuditState {
    workspace_root: String,
    leader: Option<String>,
    running: Vec<LiveSessionSummary>,
    selected: usize,
    tab: LiveTab,
    live_scroll: u16,
    status_list_lines: Vec<String>,
    event_lines: Vec<String>,
    transcript_lines: Vec<String>,
    action_lines: Vec<String>,
}

impl LiveAuditState {
    fn new(workspace_root: String) -> Self {
        Self {
            workspace_root,
            leader: None,
            running: Vec::new(),
            selected: 0,
            tab: LiveTab::Status,
            live_scroll: 0,
            status_list_lines: vec!["Waiting for activity...".to_owned()],
            event_lines: vec!["Waiting for activity...".to_owned()],
            transcript_lines: vec!["Waiting for session output...".to_owned()],
            action_lines: vec!["Waiting for runtime actions...".to_owned()],
        }
    }

    fn refresh(&mut self, loaded: &LoadedConfig, _br: &CliBrClient) -> Result<()> {
        let db = Database::open(loaded.paths.db_path())
            .with_context(|| format!("open database at {}", loaded.paths.db_path()))?;
        let bead_records = db.list_bead_records()?;

        self.leader = db
            .active_leader_lease(&chrono::Utc::now())?
            .map(|leader| leader.owner_label);

        let ready_count = bead_records
            .iter()
            .filter(|bead| bead.grove_status == GroveBeadStatus::Ready)
            .count();
        let checkpointed_count = bead_records
            .iter()
            .filter(|bead| bead.grove_status == GroveBeadStatus::Checkpointed)
            .count();
        let failed = bead_records
            .iter()
            .filter(|bead| matches!(bead.grove_status, GroveBeadStatus::Failed | GroveBeadStatus::WaitingToRetry))
            .collect::<Vec<_>>();

        self.running = bead_records
            .iter()
            .filter(|bead| bead.grove_status == GroveBeadStatus::Running)
            .map(|bead| {
                let latest_session = bead
                    .last_run_id
                    .as_ref()
                    .and_then(|run_id| db.latest_session_for_run(run_id).ok().flatten());
                LiveSessionSummary {
                    bead_id: bead.bead.id.clone(),
                    title: bead.bead.title.clone(),
                    run_id: bead.last_run_id.as_ref().map(ToString::to_string),
                    session_id: latest_session.as_ref().map(|session| session.id.to_string()),
                    transcript_path: latest_session.as_ref().map(|session| session.transcript_path.clone()),
                    started_at: latest_session.as_ref().map(|session| session.started_at.to_rfc3339()),
                    run_status: bead.last_run_id.as_ref().and_then(|run_id| db.generate_run_report(run_id).ok().flatten().map(|report| report.status)),
                    activity: bead
                        .last_run_id
                        .as_ref()
                        .and_then(|run_id| run_activity(&db, run_id.as_str()).ok().flatten()),
                    failure_detail: bead.last_failure_detail.clone(),
                }
            })
            .collect();

        self.status_list_lines = minimal_status_list_lines(&bead_records, &failed);
        self.event_lines = vec![
            format!("Running: {}", self.running.len()),
            format!("Ready: {ready_count}"),
            format!("Checkpointed: {checkpointed_count}"),
            format!("Failed: {}", failed.len()),
        ];
        self.action_lines = build_live_action_lines(&db)?;

        if self.running.is_empty() {
            self.selected = 0;
            self.live_scroll = 0;
            self.transcript_lines = vec![
                "No running sessions.".to_owned(),
                "Use Status to review ready and failed beads.".to_owned(),
                "Press q to hide the live UI while Grove keeps running.".to_owned(),
            ];
            return Ok(());
        }

        if self.selected >= self.running.len() {
            self.selected = 0;
            self.live_scroll = 0;
        }

        let selected = &self.running[self.selected];
        self.transcript_lines = build_live_content_lines(&db, selected)?;
        let max_scroll = self.transcript_lines.len().saturating_sub(1) as u16;
        self.live_scroll = self.live_scroll.min(max_scroll);
        Ok(())
    }
}

fn minimal_status_list_lines(beads: &[GroveBeadRecord], failed: &[&GroveBeadRecord]) -> Vec<String> {
    let mut lines = Vec::new();

    let running = beads
        .iter()
        .filter(|bead| bead.grove_status == GroveBeadStatus::Running)
        .collect::<Vec<_>>();
    let ready = beads
        .iter()
        .filter(|bead| bead.grove_status == GroveBeadStatus::Ready)
        .collect::<Vec<_>>();

    if !running.is_empty() {
        lines.push("Running".to_owned());
        for bead in running.iter().take(8) {
            lines.push(format!(
                "{} [{}] {}",
                bead.bead.id,
                format_priority(bead.bead.priority),
                bead.bead.title
            ));
        }
    }

    if !ready.is_empty() {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.push("Ready".to_owned());
        for bead in ready.iter().take(8) {
            lines.push(format!(
                "{} [{}] {}",
                bead.bead.id,
                format_priority(bead.bead.priority),
                bead.bead.title
            ));
        }
    }

    if !failed.is_empty() {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.push("Failed".to_owned());
        for bead in failed.iter().take(8) {
            lines.push(format!(
                "{} [{}] {}",
                bead.bead.id,
                format_priority(bead.bead.priority),
                bead.bead.title
            ));
        }
    }

    if lines.is_empty() {
        lines.push("No running, ready, or failed beads.".to_owned());
    }

    lines
}

fn run_activity(db: &Database, run_id: &str) -> Result<Option<AgentActivity>> {
    db.connection()
        .query_row(
            "SELECT activity FROM task_runs WHERE id = ?1",
            [run_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .context("query run activity")?
        .flatten()
        .map(|activity: String| match activity.to_ascii_lowercase().as_str() {
            "active" => Ok(AgentActivity::Active),
            "ready" => Ok(AgentActivity::Ready),
            "idle" => Ok(AgentActivity::Idle),
            "blocked" => Ok(AgentActivity::Blocked),
            "exited" => Ok(AgentActivity::Exited),
            other => bail!("unknown activity value {other}"),
        })
        .transpose()
}

fn build_live_action_lines(db: &Database) -> Result<Vec<String>> {
    let mut stmt = db.connection().prepare(
        "SELECT kind, bead_id, run_id, payload_json, created_at FROM event_log WHERE kind NOT IN ('LeaseHeartbeat') ORDER BY id DESC LIMIT 12",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;

    let mut lines = Vec::new();
    for row in rows {
        let (kind, bead_id, run_id, payload_json, created_at) = row?;
        let payload: serde_json::Value =
            serde_json::from_str(&payload_json).unwrap_or(serde_json::Value::Null);
        let bead = bead_id.unwrap_or_else(|| "-".to_owned());
        let run = run_id.unwrap_or_else(|| "-".to_owned());
        let summary = summarize_live_event(&kind, &payload);
        lines.push(format!("{created_at} | {bead} | {run} | {summary}"));
    }
    if lines.is_empty() {
        lines.push("No runtime actions yet.".to_owned());
    }
    Ok(lines)
}

fn summarize_event_kind(kind: EventKind, payload: &serde_json::Value) -> String {
    match kind {
        EventKind::LeaseAcquired => "leader lease acquired".to_owned(),
        EventKind::LeaseReleased => "leader lease released".to_owned(),
        EventKind::RunStarted => "run started".to_owned(),
        EventKind::RunSucceeded => "run succeeded".to_owned(),
        EventKind::RunFailed => payload
            .get("failure_detail")
            .and_then(|v| v.as_str())
            .map(|s| format!("run failed: {s}"))
            .unwrap_or_else(|| "run failed".to_owned()),
        EventKind::SessionStarted => "session started".to_owned(),
        EventKind::SessionSucceeded => "session succeeded".to_owned(),
        EventKind::SessionFailed => payload
            .get("stop_reason")
            .and_then(|v| v.as_str())
            .map(|s| format!("session failed: {s}"))
            .unwrap_or_else(|| "session failed".to_owned()),
        EventKind::ActivityStateChanged => payload
            .get("activity")
            .and_then(|v| v.as_str())
            .map(|activity| {
                let detail = payload.get("detail").and_then(|v| v.as_str()).unwrap_or("-");
                format!("activity → {activity} ({detail})")
            })
            .unwrap_or_else(|| "activity changed".to_owned()),
        EventKind::RecoveryActionTaken => payload
            .get("action")
            .and_then(|v| v.as_str())
            .map(|s| format!("recovery: {s}"))
            .unwrap_or_else(|| "recovery action".to_owned()),
        EventKind::CoordinatorStopped => payload
            .get("stop_reason")
            .and_then(|v| v.as_str())
            .map(|s| format!("coordinator stopped: {s}"))
            .unwrap_or_else(|| "coordinator stopped".to_owned()),
        EventKind::RecoveryCapsuleCreated => payload
            .get("summary")
            .and_then(|v| v.as_str())
            .map(|s| format!("recovery capsule: {s}"))
            .unwrap_or_else(|| "recovery capsule created".to_owned()),
        EventKind::SessionTerminationRequested => "session termination requested".to_owned(),
        EventKind::SessionTerminationForced => "session termination forced".to_owned(),
        EventKind::EscalationTierChanged => "escalation tier changed".to_owned(),
        EventKind::EscalationTierReset => "escalation tier reset".to_owned(),
        EventKind::HandoffWritten => "handoff written".to_owned(),
        EventKind::RunCheckpointed => "run checkpointed".to_owned(),
        EventKind::SessionCheckpointed => "session checkpointed".to_owned(),
        EventKind::ShutdownRequested => "shutdown requested".to_owned(),
        EventKind::ReactionInvoked => "reaction invoked".to_owned(),
        EventKind::ReservationGranted => "reservation granted".to_owned(),
        EventKind::ReservationConflictDetected => "reservation conflict detected".to_owned(),
        EventKind::ReservationExpired => "reservation expired".to_owned(),
        EventKind::BrMirrorRequested => "br mirror requested".to_owned(),
        EventKind::BrMirrorSucceeded => "br mirror succeeded".to_owned(),
        EventKind::BrMirrorFailed => "br mirror failed".to_owned(),
        EventKind::ArchiveIngested => "archive ingested".to_owned(),
        EventKind::PlaybookBulletAdded => "playbook bullet added".to_owned(),
        EventKind::PlaybookBulletPromoted => "playbook bullet promoted".to_owned(),
        EventKind::PlaybookBulletDeprecated => "playbook bullet deprecated".to_owned(),
        EventKind::BeadCacheSynced => "bead cache synced".to_owned(),
        EventKind::DependencySnapshotSynced => "dependency snapshot synced".to_owned(),
        EventKind::GroveStatusUpdated => "grove status updated".to_owned(),
        EventKind::LeaseHeartbeat => "leader lease heartbeat".to_owned(),
    }
}

fn summarize_live_event(kind: &str, payload: &serde_json::Value) -> String {
    match kind {
        "LeaseAcquired" => summarize_event_kind(EventKind::LeaseAcquired, payload),
        "LeaseReleased" => summarize_event_kind(EventKind::LeaseReleased, payload),
        "RunStarted" => summarize_event_kind(EventKind::RunStarted, payload),
        "RunSucceeded" => summarize_event_kind(EventKind::RunSucceeded, payload),
        "RunFailed" => summarize_event_kind(EventKind::RunFailed, payload),
        "SessionStarted" => summarize_event_kind(EventKind::SessionStarted, payload),
        "SessionSucceeded" => summarize_event_kind(EventKind::SessionSucceeded, payload),
        "SessionFailed" => summarize_event_kind(EventKind::SessionFailed, payload),
        "ActivityStateChanged" => summarize_event_kind(EventKind::ActivityStateChanged, payload),
        "RecoveryActionTaken" => summarize_event_kind(EventKind::RecoveryActionTaken, payload),
        "CoordinatorStopped" => summarize_event_kind(EventKind::CoordinatorStopped, payload),
        "RecoveryCapsuleCreated" => summarize_event_kind(EventKind::RecoveryCapsuleCreated, payload),
        "SessionTerminationRequested" => {
            summarize_event_kind(EventKind::SessionTerminationRequested, payload)
        }
        "SessionTerminationForced" => summarize_event_kind(EventKind::SessionTerminationForced, payload),
        "EscalationTierChanged" => summarize_event_kind(EventKind::EscalationTierChanged, payload),
        "EscalationTierReset" => summarize_event_kind(EventKind::EscalationTierReset, payload),
        "HandoffWritten" => summarize_event_kind(EventKind::HandoffWritten, payload),
        _ => kind.to_ascii_lowercase().replace('_', " "),
    }
}

fn build_live_content_lines(db: &Database, session: &LiveSessionSummary) -> Result<Vec<String>> {
    let mut lines = Vec::new();

    if let Some(path) = session.transcript_path.as_deref() {
        lines.extend(read_live_transcript_lines(path)?);
    } else {
        lines.push("Transcript not available yet.".to_owned());
    }

    if let Some(run_id) = session.run_id.as_deref() {
        let run_id = grove_types::RunId::new(run_id.to_owned());
        let events = db.list_events_for_run(&run_id)?;
        if !events.is_empty() {
            if !lines.is_empty() {
                lines.push(String::new());
            }
            lines.push("---- Recent actions ----".to_owned());
            for event in events.into_iter().rev().take(12).rev() {
                lines.push(format!(
                    "{} | {}",
                    event.created_at,
                    summarize_event_kind(event.kind, &event.payload)
                ));
            }
        }
    }

    if lines.is_empty() {
        lines.push("No live content yet.".to_owned());
    }

    Ok(lines)
}

fn read_live_transcript_lines(path: &str) -> Result<Vec<String>> {
    let transcript_path = std::path::Path::new(path);
    if !transcript_path.exists() {
        return Ok(vec!["Transcript not available yet.".to_owned()]);
    }

    let replay = match replay_transcript(path) {
        Ok(replay) => replay,
        Err(_) => {
            return Ok(vec![
                "Transcript is still being written...".to_owned(),
                "Retrying on next refresh.".to_owned(),
            ])
        }
    };
    let mut lines = replay
        .events
        .into_iter()
        .filter_map(|event| match event {
            TranscriptEvent::StdoutLine { line, .. } => Some(format!("OUT  {line}")),
            TranscriptEvent::StderrLine { line, .. } => Some(format!("ERR  {line}")),
            TranscriptEvent::ParsedProtocol { event, .. } => Some(format!(
                "PROTO {}",
                match event {
                    ProtocolEvent::Result { summary } => format!("result: {summary}"),
                    ProtocolEvent::Artifacts { items } => format!("artifacts: {}", items.join(", ")),
                    ProtocolEvent::Lessons { items } => format!("lessons: {}", items.join(", ")),
                    ProtocolEvent::Decisions { items } => format!("decisions: {}", items.join(", ")),
                    ProtocolEvent::Warnings { items } => format!("warnings: {}", items.join(", ")),
                    ProtocolEvent::Exit { value } => format!("exit: {value}"),
                    ProtocolEvent::Checkpoint { payload } => format!("checkpoint: {} -> {}", payload.progress, payload.next_step),
                }
            )),
            TranscriptEvent::SessionStarted { session_id, .. } => {
                Some(format!("SESSION started {}", session_id.as_str()))
            }
            TranscriptEvent::SessionEnded { exit_code, .. } => {
                Some(format!("SESSION ended {:?}", exit_code))
            }
        })
        .collect::<Vec<_>>();
    if lines.is_empty() {
        lines.push("Transcript is empty.".to_owned());
    }
    Ok(lines)
}

fn run_dispatch_loop_with_live_ui(
    loaded: &LoadedConfig,
    br: &CliBrClient,
    lease_config: &LeaderLeaseConfig,
    loop_config: &DispatchLoopConfig,
) -> Result<Result<DispatchLoopOutcome>> {
    let backend = grove_session::CliClaudeBackend::new(loaded.config.runtime.claude_bin.clone());
    let mut worker_db = Database::open(loaded.paths.db_path())
        .with_context(|| format!("open database at {}", loaded.paths.db_path()))?;
    let br_for_thread = br.clone();
    let config = loaded.config.clone();
    let lease_config = lease_config.clone();
    let loop_config = loop_config.clone();
    let (tx, rx) = mpsc::channel();

    thread::spawn(move || {
        let result = run_dispatch_loop(
            &mut worker_db,
            &backend,
            &br_for_thread,
            &config,
            &lease_config,
            &loop_config,
        );
        let _ = tx.send(result);
    });

    if !io::stdout().is_terminal() {
        return rx.recv().context("receive dispatch result from worker thread");
    }

    let mut live_hidden = false;
    let mut terminal = Some(enter_live_terminal()?);
    let mut state = LiveAuditState::new(loaded.paths.workspace_root().to_string());
    let mut completed: Option<Result<DispatchLoopOutcome>> = None;

    loop {
        if completed.is_none() && let Ok(result) = rx.try_recv() {
            completed = Some(result);
            if live_hidden {
                return Ok(completed.expect("completed result must exist"));
            }
        }

        if !live_hidden {
            state.refresh(loaded, br)?;
            let draw_result = terminal
                .as_mut()
                .context("live audit terminal missing while UI is visible")?
                .draw(|frame| draw_live_audit(frame, &state));
            if let Err(error) = draw_result {
                let detail = error.to_string();
                hide_live_ui(&mut terminal, "draw_error", Some(&detail));
                eprintln!(
                    "grove: live UI hid after a terminal draw error; dispatch is still running in the background ({detail})"
                );
                live_hidden = true;
            }
        }

        if !live_hidden {
            match event::poll(Duration::from_millis(150)) {
                Ok(true) => match event::read() {
                    Ok(CEvent::Key(key)) => match key.code {
                        KeyCode::Char('q') => {
                            hide_live_ui(&mut terminal, "user_hidden", None);
                            eprintln!(
                                "grove: live UI hidden; dispatch is still running in the background"
                            );
                            if let Some(result) = completed.take() {
                                return Ok(result);
                            }
                            live_hidden = true;
                        }
                        KeyCode::Tab => {
                            state.tab = match state.tab {
                                LiveTab::Status => LiveTab::Live,
                                LiveTab::Live => LiveTab::Status,
                            };
                        }
                        KeyCode::Right => state.tab = LiveTab::Live,
                        KeyCode::Left => state.tab = LiveTab::Status,
                        KeyCode::Down => match state.tab {
                            LiveTab::Status => {
                                if !state.running.is_empty() {
                                    state.selected = (state.selected + 1).min(state.running.len() - 1);
                                    state.live_scroll = 0;
                                }
                            }
                            LiveTab::Live => {
                                state.live_scroll = state.live_scroll.saturating_add(1);
                            }
                        },
                        KeyCode::Up => match state.tab {
                            LiveTab::Status => {
                                if state.selected > 0 {
                                    state.selected -= 1;
                                    state.live_scroll = 0;
                                }
                            }
                            LiveTab::Live => {
                                state.live_scroll = state.live_scroll.saturating_sub(1);
                            }
                        },
                        KeyCode::PageDown => {
                            if matches!(state.tab, LiveTab::Live) {
                                state.live_scroll = state.live_scroll.saturating_add(10);
                            }
                        }
                        KeyCode::PageUp => {
                            if matches!(state.tab, LiveTab::Live) {
                                state.live_scroll = state.live_scroll.saturating_sub(10);
                            }
                        }
                        KeyCode::Home => {
                            if matches!(state.tab, LiveTab::Live) {
                                state.live_scroll = 0;
                            }
                        }
                        KeyCode::End => {
                            if matches!(state.tab, LiveTab::Live) {
                                state.live_scroll = u16::MAX;
                            }
                        }
                        _ => {}
                    },
                    Ok(_) => {}
                    Err(error) => {
                        let detail = error.to_string();
                        hide_live_ui(&mut terminal, "read_error", Some(&detail));
                        eprintln!(
                            "grove: live UI hid after a terminal input error; dispatch is still running in the background ({detail})"
                        );
                        live_hidden = true;
                    }
                },
                Ok(false) => {}
                Err(error) => {
                    let detail = error.to_string();
                    hide_live_ui(&mut terminal, "poll_error", Some(&detail));
                    eprintln!(
                        "grove: live UI hid after a terminal input error; dispatch is still running in the background ({detail})"
                    );
                    live_hidden = true;
                }
            }
        } else {
            if let Some(result) = completed.take() {
                return Ok(result);
            }
            thread::sleep(Duration::from_millis(150));
        }
    }
}

fn enter_live_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode().context("enable raw mode for live audit")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    Terminal::new(backend).context("create live audit terminal")
}

fn leave_live_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    disable_raw_mode().context("disable raw mode for live audit")?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen).context("leave alternate screen")?;
    terminal.show_cursor().context("restore terminal cursor")
}

fn hide_live_ui(
    terminal: &mut Option<Terminal<CrosstermBackend<io::Stdout>>>,
    reason: &str,
    detail: Option<&str>,
) {
    if let Some(mut terminal) = terminal.take() {
        if let Err(error) = leave_live_terminal(&mut terminal) {
            trace_runtime_event(
                "coordinator.live_ui_hide_error",
                serde_json::json!({
                    "reason": reason,
                    "detail": detail,
                    "cleanup_error": error.to_string(),
                }),
            );
            return;
        }
    }

    trace_runtime_event(
        "coordinator.live_ui_hidden",
        serde_json::json!({
            "reason": reason,
            "detail": detail,
        }),
    );
}

fn draw_live_audit(frame: &mut Frame<'_>, state: &LiveAuditState) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(8),
        ])
        .split(frame.area());

    let leader = state.leader.as_deref().unwrap_or("none");
    let header = Paragraph::new(vec![Line::from(vec![
        Span::styled("grove --live", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(format!("  workspace={}  leader={leader}", state.workspace_root)),
    ])])
    .block(Block::default().borders(Borders::ALL).title("Live Audit"));
    frame.render_widget(header, layout[0]);

    let tabs = Tabs::new(vec!["Status", "Live Session"])
        .select(match state.tab {
            LiveTab::Status => 0,
            LiveTab::Live => 1,
        })
        .block(Block::default().borders(Borders::ALL).title("Tabs"));
    frame.render_widget(tabs, layout[1]);

    match state.tab {
        LiveTab::Status => draw_status_tab(frame, layout[2], state),
        LiveTab::Live => draw_live_tab(frame, layout[2], state),
    }
}

fn draw_status_tab(frame: &mut Frame<'_>, area: Rect, state: &LiveAuditState) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(area);

    let items = state
        .status_list_lines
        .iter()
        .map(|line| ListItem::new(line.clone()))
        .collect::<Vec<_>>();
    let running = List::new(items).block(Block::default().borders(Borders::ALL).title("Beads"));
    frame.render_widget(running, chunks[0]);

    let summary = Paragraph::new(state.event_lines.join("\n"))
        .block(Block::default().borders(Borders::ALL).title("Status"))
        .wrap(Wrap { trim: false });
    frame.render_widget(summary, chunks[1]);
}

fn draw_live_tab(frame: &mut Frame<'_>, area: Rect, state: &LiveAuditState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(10), Constraint::Min(8)])
        .split(area);

    let details = state
        .running
        .get(state.selected)
        .map(|session| {
            let mut lines = vec![
                Line::from(format!("bead: {}", session.bead_id)),
                Line::from(format!("title: {}", session.title)),
                Line::from(format!("run: {}", session.run_id.as_deref().unwrap_or("-"))),
                Line::from(format!("session: {}", session.session_id.as_deref().unwrap_or("-"))),
                Line::from(format!("started: {}", session.started_at.as_deref().unwrap_or("-"))),
                Line::from(format!("run status: {}", display_option(session.run_status.map(|status| format!("{status:?}"))))),
                Line::from(format!("activity: {}", display_option(session.activity.map(|activity| format!("{activity:?}"))))),
            ];
            if let Some(detail) = session.failure_detail.as_deref() {
                lines.push(Line::from(format!("detail: {detail}")));
            }
            lines
        })
        .unwrap_or_else(|| {
            vec![
                Line::from("No running session selected."),
                Line::from("Switch back to Status to review ready/failed beads."),
                Line::from("This tab will show live Claude output once a session starts."),
            ]
        });
    let details = Paragraph::new(details)
        .block(Block::default().borders(Borders::ALL).title("Current Session"));
    frame.render_widget(details, chunks[0]);

    let transcript = Paragraph::new(state.transcript_lines.join("\n"))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Live Content (↑/↓ scroll, PgUp/PgDn jump)"),
        )
        .scroll((state.live_scroll, 0))
        .wrap(Wrap { trim: false });
    frame.render_widget(transcript, chunks[1]);
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

fn resolve_init_paths(workspace_root: &Utf8Path, config_path: &Utf8Path) -> Result<GrovePaths> {
    if config_path.exists() {
        return load_from_workspace(workspace_root)
            .with_context(|| format!("load grove configuration from {config_path}"))
            .map(|loaded| loaded.paths);
    }

    let config = grove_config::GroveConfig::default();
    GrovePaths::from_config(&config, config_path).map_err(|error| anyhow!(error.to_string()))
}

fn existing_init_artifacts(paths: &GrovePaths) -> Vec<String> {
    paths
        .initialization_markers()
        .into_iter()
        .filter_map(|(label, path)| {
            path.exists().then_some(format!("{label}={}", path.as_str()))
        })
        .collect()
}

fn reset_managed_init_state(paths: &GrovePaths) -> Result<()> {
    for (_label, path) in paths.managed_reset_paths() {
        let std_path = path.as_std_path();
        if !std_path.exists() {
            continue;
        }
        if std_path.is_dir() {
            fs::remove_dir_all(std_path)
                .with_context(|| format!("remove Grove-managed directory {}", path))?;
        } else {
            fs::remove_file(std_path)
                .with_context(|| format!("remove Grove-managed file {}", path))?;
        }
    }
    Ok(())
}

fn write_default_config(path: &Utf8PathBuf) -> Result<()> {
    let text = DEFAULT_INIT_GROVE_TOML.trim_end();
    fs::write(path, format!("{text}\n"))
        .with_context(|| format!("write default config to {path}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::error::Error;
    use tempfile::tempdir;

    type TestResult = Result<(), Box<dyn Error>>;

    #[test]
    fn live_transcript_tail_tolerates_partial_jsonl_writes() -> TestResult {
        let dir = tempdir()?;
        let path = dir.path().join("partial.jsonl");
        fs::write(
            &path,
            concat!(
                "{\"ts\":\"2026-03-23T00:00:00Z\",\"kind\":\"session_started\",\"session_id\":\"ses-1\"}\n",
                "{\"ts\":\"2026-03-23T00:00:01Z\",\"kind\":\"stdout\",\"line\":"
            ),
        )?;

        let lines = read_live_transcript_lines(path.to_str().unwrap())?;
        assert_eq!(
            lines,
            vec![
                "Transcript is still being written...".to_owned(),
                "Retrying on next refresh.".to_owned(),
            ]
        );
        Ok(())
    }
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
            println!(
                "Last coordinator stop: {} at {}",
                stop.reason, stop.created_at
            );
            println!(
                "Coordinator forced termination: {}",
                if stop.forced { "yes" } else { "no" }
            );
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

            println!("\nRun report:");
            match view.run_report.as_ref() {
                Some(report) => {
                    print_run_report(report);
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

fn print_run_report(report: &RunReport) {
    println!("- run: {}", report.run_id);
    println!("- bead: {}", report.bead_id);
    println!("- status: {:?}", report.status);
    if let Some(failure_class) = &report.failure_class {
        println!("- failure class: {:?}", failure_class);
    }
    let m = &report.metrics;
    println!("- duration: {}s", m.total_duration_secs);
    println!("- checkpoints: {}", m.checkpoints_taken);
    println!("- retries: {}", m.retries_attempted);
    println!("- rescue injections: {}", m.rescue_injections);
    println!("- reactions invoked: {}", m.reactions_invoked);
    println!("- max escalation tier: {}", m.max_escalation_tier);
    if let Some(reason) = &m.termination_reason {
        println!("- termination reason: {}", reason);
    }
    println!("- events: {}", report.event_count);
    println!("- first event: {}", display_option(report.first_event_at));
    println!("- last event: {}", display_option(report.last_event_at));
    if report.recovery_capsule.is_some() {
        println!("- recovery capsule: present");
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
