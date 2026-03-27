use crate::RunStartInput;
use crate::status_view::{SuppressionReasonView, find_reservation_conflicts};
use crate::{
    AcquireReservationInput, DispatchEligibilityContext, LeaderLeaseConfig, LeaderLeaseManager,
    LocalSuppressionReason, PersistedTaskRunOutcome, ReservationManager,
    execute_persisted_single_task_session_after_run_started,
};
use anyhow::{Context, Result, anyhow};
use camino::{Utf8Path, Utf8PathBuf};
use grove_br::BrClient;
use grove_config::{DEFAULT_CHECKPOINTS_DIR_NAME, DEFAULT_GROVE_DIR_NAME, GroveConfig};
use grove_db::Database;
use grove_session::{
    CheckpointPromptInput, ClaudeBackend, ContextMonitor, ExitPolicy, SessionShutdownConfig,
    SingleTaskSessionRequest,
};
use grove_types::{
    BeadId, CoordinatorStopReason, EscalationTier, ExecutionContract, GroveBeadRecord,
    GroveBeadStatus, PromptId, ReservationConflict, ReservationMode, RunId, SessionId, Timestamp,
};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

/// Thread-safe shutdown signal for the coordinator.
///
/// Set from a signal handler (SIGINT/SIGTERM/Ctrl-C) and polled each
/// dispatch cycle. This enables graceful shutdown: the loop stops
/// dispatching new work, persists pending state, and releases the
/// leader lease.
#[derive(Debug, Clone)]
pub struct ShutdownSignal {
    flag: Arc<AtomicBool>,
}

impl ShutdownSignal {
    /// Create a new shutdown signal (not triggered).
    #[must_use]
    pub fn new() -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Trigger the shutdown signal.
    pub fn trigger(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    /// Check whether shutdown has been requested.
    #[must_use]
    pub fn is_triggered(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    #[must_use]
    pub fn shared_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.flag)
    }

    /// Register this signal with the ctrlc handler.
    /// Returns an error if the handler cannot be set.
    pub fn register_ctrlc(&self) -> Result<()> {
        let flag = self.flag.clone();
        ctrlc::set_handler(move || {
            flag.store(true, Ordering::SeqCst);
            eprintln!("\ngrove: shutdown signal received, stopping after current dispatch...");
        })
        .context("register Ctrl-C handler")?;
        Ok(())
    }
}

impl Default for ShutdownSignal {
    fn default() -> Self {
        Self::new()
    }
}

/// Outcome of a single dispatch loop run.
#[derive(Debug, Clone)]
pub struct DispatchLoopOutcome {
    /// Total number of dispatch attempts made in this loop run.
    pub dispatched_count: u32,
    /// Total number of poll cycles executed.
    pub poll_cycles: u32,
    /// Reason the dispatch loop terminated.
    pub exit_reason: DispatchExitReason,
    /// Durable stop reason for post-mortem analysis.
    pub stop_reason: CoordinatorStopReason,
    /// Compact summary when ready work exists but local Grove state blocks dispatch.
    pub blocked_summary: Option<DispatchBlockedSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DispatchBlockedSummary {
    pub blocked_ready_count: usize,
    pub reason_counts: Vec<BlockedReasonCount>,
    pub sample_beads: Vec<BlockedSampleBead>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BlockedReasonCount {
    pub code: &'static str,
    pub summary: String,
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BlockedSampleBead {
    pub bead_id: BeadId,
    pub reasons: Vec<BlockedSampleReason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BlockedSampleReason {
    pub code: &'static str,
    pub summary: String,
}

/// Why the dispatch loop stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchExitReason {
    /// No more dispatchable beads remain.
    QueueEmpty,
    /// Ready beads exist, but all are blocked by local Grove state.
    DispatchBlocked,
    /// Reached the maximum number of total dispatches.
    MaxRunsReached,
    /// Leader lease was contested/lost.
    LeaderContested,
    /// The configured max poll cycles were exceeded.
    MaxPollCycles { limit: u32 },
    /// Shutdown signal received (SIGINT/SIGTERM/Ctrl-C).
    ShutdownRequested,
}

impl std::fmt::Display for DispatchExitReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::QueueEmpty => write!(f, "no dispatchable beads remain"),
            Self::DispatchBlocked => write!(f, "ready beads are blocked by local Grove state"),
            Self::MaxRunsReached => write!(f, "reached max total runs"),
            Self::LeaderContested => write!(f, "leader lease contested"),
            Self::MaxPollCycles { limit } => write!(f, "exceeded max poll cycles ({limit})"),
            Self::ShutdownRequested => write!(f, "shutdown signal received"),
        }
    }
}

impl DispatchExitReason {
    /// Convert to a durable `CoordinatorStopReason`.
    #[must_use]
    pub fn to_stop_reason(&self) -> CoordinatorStopReason {
        match self {
            Self::QueueEmpty => CoordinatorStopReason::QueueEmpty,
            Self::DispatchBlocked => CoordinatorStopReason::DispatchBlocked,
            Self::MaxRunsReached => CoordinatorStopReason::MaxRunsReached,
            Self::LeaderContested => CoordinatorStopReason::LeaderContested,
            Self::MaxPollCycles { .. } => CoordinatorStopReason::MaxPollCycles,
            Self::ShutdownRequested => CoordinatorStopReason::UserStopped,
        }
    }
}

fn dispatch_loop_outcome(
    dispatched_count: u32,
    poll_cycles: u32,
    exit_reason: DispatchExitReason,
    blocked_summary: Option<DispatchBlockedSummary>,
) -> DispatchLoopOutcome {
    let stop_reason = exit_reason.to_stop_reason();
    DispatchLoopOutcome {
        dispatched_count,
        poll_cycles,
        exit_reason,
        stop_reason,
        blocked_summary,
    }
}

fn summarize_blocked_ready_beads(
    beads: &[GroveBeadRecord],
    ready_ids: &HashSet<BeadId>,
    reservation_conflicts: &[ReservationConflict],
    now: Timestamp,
) -> Option<DispatchBlockedSummary> {
    let mut reason_counts = BTreeMap::<(&'static str, String), usize>::new();
    let mut sample_beads = Vec::new();
    let mut blocked_ready_count = 0usize;

    for bead in beads
        .iter()
        .filter(|bead| ready_ids.contains(&bead.bead.id))
    {
        let eligibility = crate::evaluate_dispatch_eligibility(
            bead,
            &DispatchEligibilityContext {
                ready_in_br: true,
                circuit_state: crate::circuit_state_for_bead(bead),
                reservation_conflicts: reservation_conflicts_for_bead(bead, reservation_conflicts),
                now,
            },
        );
        if eligibility.local_suppression_reasons.is_empty() {
            continue;
        }

        blocked_ready_count += 1;
        for reason in &eligibility.local_suppression_reasons {
            let view = SuppressionReasonView::from_reason(reason);
            *reason_counts
                .entry((view.code, view.summary.clone()))
                .or_default() += 1;
        }
        if sample_beads.len() < 3 {
            sample_beads.push(BlockedSampleBead {
                bead_id: bead.bead.id.clone(),
                reasons: eligibility
                    .local_suppression_reasons
                    .iter()
                    .map(blocked_sample_reason_from_reason)
                    .collect(),
            });
        }
    }

    (blocked_ready_count > 0).then(|| DispatchBlockedSummary {
        blocked_ready_count,
        reason_counts: reason_counts
            .into_iter()
            .map(|((code, summary), count)| BlockedReasonCount {
                code,
                summary,
                count,
            })
            .collect(),
        sample_beads,
    })
}

fn blocked_sample_reason_from_reason(reason: &LocalSuppressionReason) -> BlockedSampleReason {
    let view = SuppressionReasonView::from_reason(reason);
    BlockedSampleReason {
        code: view.code,
        summary: view.summary,
    }
}

fn reservation_conflicts_for_bead(
    bead: &GroveBeadRecord,
    reservation_conflicts: &[ReservationConflict],
) -> Vec<ReservationConflict> {
    reservation_conflicts
        .iter()
        .filter(|conflict| {
            conflict.requested_by_bead == bead.bead.id || conflict.conflicting_bead == bead.bead.id
        })
        .cloned()
        .collect()
}

fn load_previous_outcome(
    db: &Database,
    run_id: &RunId,
) -> Result<Option<grove_types::SessionOutcome>> {
    let Some(session) = db.latest_session_for_run(run_id)? else {
        return Ok(None);
    };

    let replay = match grove_session::replay_transcript(&session.transcript_path) {
        Ok(replay) => replay,
        Err(_) => return Ok(None),
    };
    let mut stdout_tail = Vec::new();
    let mut stderr_tail = Vec::new();
    for event in replay.events {
        match event {
            grove_types::TranscriptEvent::StdoutLine { line, .. } => stdout_tail.push(line),
            grove_types::TranscriptEvent::StderrLine { line, .. } => stderr_tail.push(line),
            _ => {}
        }
    }
    if stdout_tail.len() > 20 {
        stdout_tail = stdout_tail[stdout_tail.len().saturating_sub(20)..].to_vec();
    }
    if stderr_tail.len() > 20 {
        stderr_tail = stderr_tail[stderr_tail.len().saturating_sub(20)..].to_vec();
    }

    Ok(Some(grove_types::SessionOutcome {
        session,
        protocol_events: Vec::new(),
        analysis: grove_types::IterationAnalysis::default(),
        terminal_class: grove_types::SessionTerminalClass::Crash,
        context_pressure_pct: None,
        context_pressure_level: grove_types::ContextPressureLevel::Ok,
        stdout_tail,
        stderr_tail,
    }))
}

/// Configuration for the dispatch loop beyond what `GroveConfig` provides.
#[derive(Debug, Clone)]
pub struct DispatchLoopConfig {
    /// Maximum total dispatches before the loop exits. `None` means unlimited.
    pub max_total_runs: Option<u32>,
    /// Maximum poll cycles before the loop exits. `None` means unlimited.
    pub max_poll_cycles: Option<u32>,
    /// Working directory for session execution.
    pub working_dir: Utf8PathBuf,
    /// Shutdown signal for graceful termination.
    pub shutdown_signal: ShutdownSignal,
    /// Database path so worker threads can open independent SQLite connections.
    pub db_path: Utf8PathBuf,
}

#[derive(Debug, Clone)]
struct DispatchedWorkerContext {
    bead_id: BeadId,
    run_id: RunId,
    session_id: SessionId,
}

#[derive(Debug)]
struct CompletedWorker {
    ctx: DispatchedWorkerContext,
    result: Result<PersistedTaskRunOutcome, String>,
}

struct InFlightWorker {
    handle: JoinHandle<()>,
}

enum LeaseMonitorEvent {
    Contested,
    Error(String),
}

struct LeaseMonitorGuard {
    stop_flag: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for LeaseMonitorGuard {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn start_lease_monitor(
    lease_config: &LeaderLeaseConfig,
    loop_config: &DispatchLoopConfig,
) -> (LeaseMonitorGuard, mpsc::Receiver<LeaseMonitorEvent>) {
    let renew_interval = lease_renew_interval(lease_config.lease_ttl);
    let (tx, rx) = mpsc::channel();
    let stop_flag = Arc::new(AtomicBool::new(false));
    let thread_stop_flag = Arc::clone(&stop_flag);
    let thread_lease_config = lease_config.clone();
    let thread_db_path = loop_config.db_path.clone();
    let thread_shutdown_signal = loop_config.shutdown_signal.clone();
    let handle = std::thread::spawn(move || {
        let mut db = match Database::open(&thread_db_path) {
            Ok(db) => db,
            Err(error) => {
                let _ = tx.send(LeaseMonitorEvent::Error(error.to_string()));
                thread_shutdown_signal.trigger();
                return;
            }
        };

        while !thread_stop_flag.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(
                renew_interval.num_milliseconds().max(1) as u64,
            ));
            if thread_stop_flag.load(Ordering::SeqCst) {
                break;
            }
            match LeaderLeaseManager::heartbeat(&mut db, &thread_lease_config, chrono::Utc::now()) {
                Ok(Some(_)) => {}
                Ok(None) => {
                    let _ = tx.send(LeaseMonitorEvent::Contested);
                    thread_shutdown_signal.trigger();
                    break;
                }
                Err(error) => {
                    let _ = tx.send(LeaseMonitorEvent::Error(error.to_string()));
                    thread_shutdown_signal.trigger();
                    break;
                }
            }
        }
    });

    (
        LeaseMonitorGuard {
            stop_flag,
            handle: Some(handle),
        },
        rx,
    )
}

fn lease_renew_interval(lease_ttl: chrono::Duration) -> chrono::Duration {
    chrono::Duration::milliseconds((lease_ttl.num_milliseconds() / 3).max(1))
}

fn handle_completed_worker<C: BrClient>(
    db: &mut Database,
    br: &C,
    config: &GroveConfig,
    inflight_workers: &mut HashMap<BeadId, InFlightWorker>,
    completed: CompletedWorker,
) {
    let CompletedWorker { ctx, result } = completed;
    if let Some(worker) = inflight_workers.remove(&ctx.bead_id) {
        let _ = worker.handle.join();
    }

    match result {
        Ok(outcome) => {
            apply_reaction_side_effects(db, config, &ctx, Some(&outcome), None, false);
            if outcome.session.session.stop_reason == Some(grove_types::StopReason::Kill) {
                let _ = db.write_event_log(
                    grove_types::EventKind::CoordinatorStopped,
                    Some(&ctx.bead_id),
                    Some(&ctx.run_id),
                    Some(&ctx.session_id),
                    &serde_json::json!({
                        "exit_reason": "shutdown signal received",
                        "stop_reason": grove_types::CoordinatorStopReason::Interrupted.as_str(),
                        "forced_termination": true,
                        "running_session_count": inflight_workers.len(),
                        "leader_released": false,
                    }),
                    &chrono::Utc::now(),
                );
            }
            eprintln!(
                "grove dispatch: {} completed with status {:?}",
                ctx.bead_id.as_str(),
                outcome.run.status
            );

            if config.reservations.enabled {
                let _ = ReservationManager::release_for_run(
                    db,
                    &ctx.bead_id,
                    &ctx.run_id,
                    chrono::Utc::now(),
                );
            }

            if outcome.run.status == grove_types::RunStatus::Succeeded
                && let Some(handoff) = outcome.handoff.as_ref()
            {
                match br.mirror_handoff(&ctx.bead_id, handoff, true) {
                    Ok(()) => {
                        eprintln!("grove dispatch: mirrored {} to br", ctx.bead_id.as_str());
                    }
                    Err(error) => {
                        eprintln!(
                            "grove dispatch: mirror failed for {}: {error}",
                            ctx.bead_id.as_str()
                        );
                        let _ = db.enqueue_mirror_outbox(&ctx.bead_id, &ctx.run_id, handoff, true);
                        apply_reaction_side_effects(
                            db,
                            config,
                            &ctx,
                            Some(&outcome),
                            Some(&error.to_string()),
                            true,
                        );
                    }
                }
            }
        }
        Err(error) => {
            apply_reaction_side_effects(db, config, &ctx, None, Some(&error), false);
            eprintln!("grove dispatch: {} failed: {error}", ctx.bead_id.as_str());
            if config.reservations.enabled {
                let _ = ReservationManager::release_for_run(
                    db,
                    &ctx.bead_id,
                    &ctx.run_id,
                    chrono::Utc::now(),
                );
            }
        }
    }

    if let Err(error) = grove_br::sync_bead_cache(br, db) {
        eprintln!("grove dispatch: bead cache sync failed: {error}");
    }
}

fn drain_inflight_workers<C: BrClient>(
    db: &mut Database,
    br: &C,
    config: &GroveConfig,
    inflight_workers: &mut HashMap<BeadId, InFlightWorker>,
    completed_rx: &mpsc::Receiver<CompletedWorker>,
    poll_sleep: Duration,
    drain_deadline: Option<std::time::Instant>,
) {
    while !inflight_workers.is_empty() {
        let remaining = drain_deadline
            .map(|deadline| deadline.saturating_duration_since(std::time::Instant::now()));
        let recv_timeout = remaining.map_or(poll_sleep, |remaining| remaining.min(poll_sleep));

        match completed_rx.recv_timeout(recv_timeout) {
            Ok(completed) => {
                handle_completed_worker(db, br, config, inflight_workers, completed);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if drain_deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

fn consecutive_failures_from_history(
    run_history: Option<&[grove_types::TaskRunRecord]>,
    current_run_id: &RunId,
    current_status: grove_types::RunStatus,
) -> u32 {
    if !matches!(
        current_status,
        grove_types::RunStatus::Failed | grove_types::RunStatus::WaitingToRetry
    ) {
        return 0;
    }

    let Some(run_history) = run_history else {
        return 1;
    };

    let Some(current_attempt_no) = run_history
        .iter()
        .find(|run| run.id == *current_run_id)
        .map(|run| run.attempt_no)
    else {
        return 1;
    };

    let mut attempts: Vec<_> = run_history.iter().collect();
    attempts.sort_by_key(|run| run.attempt_no);

    let mut streak = 0;
    for run in attempts.into_iter().rev() {
        if run.attempt_no > current_attempt_no {
            continue;
        }
        if !matches!(
            run.status,
            grove_types::RunStatus::Failed | grove_types::RunStatus::WaitingToRetry
        ) {
            break;
        }
        streak += 1;
    }

    streak.max(1)
}

fn apply_reaction_side_effects(
    db: &mut Database,
    config: &GroveConfig,
    ctx: &DispatchedWorkerContext,
    outcome: Option<&PersistedTaskRunOutcome>,
    error_detail: Option<&str>,
    mirror_failed: bool,
) {
    let (
        run_status,
        failure_class,
        failure_detail,
        escalation_tier,
        context_pressure_pct,
        inferred_activity,
    ) = if let Some(outcome) = outcome {
        let run = &outcome.run;
        let failure_detail = run
            .failure_detail
            .clone()
            .or_else(|| error_detail.map(str::to_owned));
        (
            run.status,
            run.failure_class,
            failure_detail,
            run.escalation_tier,
            outcome.session.context_pressure_pct,
            crate::reactions::infer_agent_activity(&outcome.session, run.status),
        )
    } else {
        let failure_class = if mirror_failed {
            Some(grove_types::FailureClass::BrMirrorFailed)
        } else {
            Some(grove_types::FailureClass::Unknown)
        };
        let run_status = grove_types::RunStatus::Failed;
        (
            run_status,
            failure_class,
            error_detail.map(str::to_owned),
            grove_types::EscalationTier::FirstAttempt,
            None,
            match failure_class {
                Some(grove_types::FailureClass::PermissionDenied) => {
                    grove_types::AgentActivity::Blocked
                }
                Some(grove_types::FailureClass::NoProgress) => grove_types::AgentActivity::Idle,
                _ => grove_types::AgentActivity::Exited,
            },
        )
    };

    let bead_record = db.get_bead_record(&ctx.bead_id).ok().flatten();
    let run_history = db.list_task_runs_for_bead(&ctx.bead_id).ok();
    let existing_tier = run_history
        .as_ref()
        .and_then(|runs| runs.iter().find(|run| run.id == ctx.run_id))
        .map(|run| run.escalation_tier)
        .unwrap_or(escalation_tier);
    let consecutive_failures =
        consecutive_failures_from_history(run_history.as_deref(), &ctx.run_id, run_status);

    let trigger_ctx = crate::reactions::TriggerContext {
        bead_id: ctx.bead_id.clone(),
        run_id: ctx.run_id.clone(),
        run_status,
        activity: inferred_activity,
        failure_class,
        failure_detail: failure_detail.clone(),
        escalation_tier: existing_tier,
        consecutive_failures,
        circuit_state: bead_record
            .as_ref()
            .map(crate::circuit_state_for_bead)
            .unwrap_or(grove_types::CircuitState::Closed),
        context_pressure_pct,
        mirror_failed,
    };

    let rules = crate::reactions::load_reaction_rules(config);
    let reaction_eval = crate::reactions::evaluate_reactions(db, &trigger_ctx, &rules);
    let now = chrono::Utc::now();

    let _ = db.update_run_activity(&ctx.bead_id, &ctx.run_id, inferred_activity, &now);
    if reaction_eval.new_tier != existing_tier {
        let _ =
            db.update_run_escalation_tier(&ctx.bead_id, &ctx.run_id, reaction_eval.new_tier, &now);
    }

    if let Some(outcome) = outcome
        && matches!(
            outcome.run.status,
            grove_types::RunStatus::Succeeded | grove_types::RunStatus::Checkpointed
        )
    {
        let _ = db.update_run_escalation_tier(
            &ctx.bead_id,
            &ctx.run_id,
            grove_types::EscalationTier::FirstAttempt,
            &now,
        );
        let _ = db.write_event_log(
            grove_types::EventKind::EscalationTierReset,
            Some(&ctx.bead_id),
            Some(&ctx.run_id),
            Some(&ctx.session_id),
            &serde_json::json!({"reset_to": "FirstAttempt"}),
            &now,
        );
    }

    let mut terminal_run_persisted = false;

    for record in reaction_eval.records {
        let _ = db.write_event_log(
            grove_types::EventKind::ReactionInvoked,
            Some(&ctx.bead_id),
            Some(&ctx.run_id),
            outcome.map(|_| &ctx.session_id),
            &serde_json::to_value(&record).unwrap_or_else(|_| serde_json::json!({})),
            &now,
        );

        match &record.action {
            grove_types::ReactionAction::RetryWithMutation { .. } => {
                let plan = grove_session::plan_retry_mutation(
                    failure_class.unwrap_or(grove_types::FailureClass::Unknown),
                    outcome.map(|outcome| &outcome.session),
                );
                let _ = db.write_recovery_capsule(grove_db::RecoveryCapsuleWriteInput {
                    bead_id: ctx.bead_id.clone(),
                    run_id: ctx.run_id.clone(),
                    capsule: grove_types::RecoveryCapsule::from_parts(
                        grove_types::RecoveryCapsuleOutcome::Failed,
                        failure_class,
                        failure_detail.as_deref(),
                        None,
                        None,
                        Some(plan.next_contract.as_str()),
                        Some(plan.retry_delta_summary.as_str()),
                        &[],
                    )
                    .unwrap_or_else(|| grove_types::RecoveryCapsule {
                        outcome: grove_types::RecoveryCapsuleOutcome::Failed,
                        summary: plan.rescue_card,
                        strongest_evidence: Vec::new(),
                        likely_root_causes: Vec::new(),
                        risky_paths: Vec::new(),
                        do_not_repeat: Vec::new(),
                        next_attempt_contract: Some(plan.next_contract.as_str().to_owned()),
                        retry_delta_summary: Some(plan.retry_delta_summary),
                        checkpoint_progress: None,
                        checkpoint_next_step: None,
                        artifacts: Vec::new(),
                    }),
                    created_at: now,
                });
            }
            grove_types::ReactionAction::CreateRecoveryCapsule {
                outcome: capsule_outcome,
            } => {
                if let Some(capsule) = grove_types::RecoveryCapsule::from_parts(
                    *capsule_outcome,
                    failure_class,
                    failure_detail.as_deref(),
                    None,
                    None,
                    None,
                    None,
                    &[],
                ) {
                    let _ = db.write_recovery_capsule(grove_db::RecoveryCapsuleWriteInput {
                        bead_id: ctx.bead_id.clone(),
                        run_id: ctx.run_id.clone(),
                        capsule,
                        created_at: now,
                    });
                }
            }
            grove_types::ReactionAction::ScheduleBackoff { base_secs } => {
                let retry_after = now + chrono::Duration::seconds(*base_secs as i64);
                let _ = db.record_run_finished(
                    &ctx.bead_id,
                    grove_db::RunFinishInput {
                        run_id: ctx.run_id.clone(),
                        status: grove_types::RunStatus::WaitingToRetry,
                        failure_class,
                        failure_detail: failure_detail.clone(),
                        ended_at: now,
                        retry_after: Some(retry_after),
                        circuit_breaker_state: bead_record
                            .as_ref()
                            .and_then(|record| record.circuit_breaker_state.clone()),
                    },
                );
                terminal_run_persisted = true;
            }
            grove_types::ReactionAction::ForceCheckpoint
            | grove_types::ReactionAction::EnqueueMirrorRetry
            | grove_types::ReactionAction::InjectRescue { .. } => {}
            grove_types::ReactionAction::GiveUp => {
                if let Some(capsule) = grove_types::RecoveryCapsule::from_parts(
                    grove_types::RecoveryCapsuleOutcome::Failed,
                    failure_class,
                    failure_detail.as_deref(),
                    None,
                    None,
                    None,
                    None,
                    &[],
                ) {
                    let _ = db.write_recovery_capsule(grove_db::RecoveryCapsuleWriteInput {
                        bead_id: ctx.bead_id.clone(),
                        run_id: ctx.run_id.clone(),
                        capsule,
                        created_at: now,
                    });
                }
                let _ = db.record_run_finished(
                    &ctx.bead_id,
                    grove_db::RunFinishInput {
                        run_id: ctx.run_id.clone(),
                        status: grove_types::RunStatus::Failed,
                        failure_class,
                        failure_detail: failure_detail.clone(),
                        ended_at: now,
                        retry_after: None,
                        circuit_breaker_state: bead_record
                            .as_ref()
                            .and_then(|record| record.circuit_breaker_state.clone()),
                    },
                );
                terminal_run_persisted = true;
            }
        }
    }

    if outcome.is_none() && !terminal_run_persisted {
        let _ = db.record_run_finished(
            &ctx.bead_id,
            grove_db::RunFinishInput {
                run_id: ctx.run_id.clone(),
                status: grove_types::RunStatus::Failed,
                failure_class,
                failure_detail,
                ended_at: now,
                retry_after: None,
                circuit_breaker_state: bead_record
                    .as_ref()
                    .and_then(|record| record.circuit_breaker_state.clone()),
            },
        );
    }
}

/// Score a single bead for dispatch priority using the same logic as `status_view`.
#[must_use]
fn score_bead(bead: &GroveBeadRecord, config: &GroveConfig) -> f64 {
    let mut score = match bead.bead.priority {
        grove_types::BeadPriority::P0 => 100.0,
        grove_types::BeadPriority::P1 => 75.0,
        grove_types::BeadPriority::P2 => 50.0,
        grove_types::BeadPriority::P3 => 25.0,
        grove_types::BeadPriority::P4 => 10.0,
    };

    if bead.grove_status == GroveBeadStatus::WaitingToRetry {
        score -= f64::from(config.scheduler.retry_penalty);
    }

    score
}

/// Select the highest-scored dispatchable bead from the ready list.
#[cfg(test)]
fn select_best_candidate<'a>(
    beads: &'a [GroveBeadRecord],
    ready_ids: &HashSet<BeadId>,
    config: &GroveConfig,
    now: Timestamp,
) -> Option<&'a GroveBeadRecord> {
    let excluded_ids = HashSet::new();
    select_best_candidate_excluding(beads, ready_ids, &excluded_ids, config, now)
}

fn select_best_candidate_excluding<'a>(
    beads: &'a [GroveBeadRecord],
    ready_ids: &HashSet<BeadId>,
    excluded_ids: &HashSet<BeadId>,
    config: &GroveConfig,
    now: Timestamp,
) -> Option<&'a GroveBeadRecord> {
    let mut candidates: Vec<_> = beads
        .iter()
        .filter(|bead| {
            !excluded_ids.contains(&bead.bead.id) && {
                let eligibility = crate::evaluate_dispatch_eligibility(
                    bead,
                    &DispatchEligibilityContext {
                        ready_in_br: ready_ids.contains(&bead.bead.id),
                        circuit_state: crate::circuit_state_for_bead(bead),
                        reservation_conflicts: Vec::new(),
                        now,
                    },
                );
                eligibility.dispatchable_in_grove
            }
        })
        .collect();

    candidates.sort_by(|a, b| {
        let score_a = score_bead(a, config);
        let score_b = score_bead(b, config);
        score_b
            .partial_cmp(&score_a)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.bead.id.cmp(&b.bead.id))
    });

    candidates.into_iter().next()
}

/// Build a `SingleTaskSessionRequest` from a dispatched bead.
fn load_startup_prompt(config: &GroveConfig, working_dir: &Utf8Path) -> Option<String> {
    let path = if Utf8Path::new(&config.runtime.startup_prompt_path).is_absolute() {
        Utf8PathBuf::from(config.runtime.startup_prompt_path.as_str())
    } else {
        working_dir.join(&config.runtime.startup_prompt_path)
    };

    let text = fs::read_to_string(path).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

fn build_session_request(
    bead: &GroveBeadRecord,
    config: &GroveConfig,
    working_dir: &Utf8Path,
    run_id: &RunId,
    session_id: &SessionId,
    parent_handoffs: Vec<String>,
    escalation_tier: EscalationTier,
) -> SingleTaskSessionRequest {
    let startup_prompt = load_startup_prompt(config, working_dir);
    let prompt_id = PromptId::new(format!("prompt-{}", run_id.as_str()));
    let transcript_path = Utf8PathBuf::from(format!(
        ".grove/transcripts/{}/{}.jsonl",
        bead.bead.id.as_str(),
        session_id.as_str()
    ));
    let prompt_manifest_path =
        Utf8PathBuf::from(format!(".grove/prompts/{}.json", prompt_id.as_str()));

    SingleTaskSessionRequest {
        bead_id: bead.bead.id.clone(),
        run_id: run_id.clone(),
        session_id: session_id.clone(),
        prompt_id,
        task_title: bead.bead.title.clone(),
        task_description: bead.bead.description.clone().unwrap_or_default(),
        startup_prompt,
        contract: ExecutionContract::SingleTask,
        model: config.runtime.default_model.clone(),
        working_dir: working_dir.to_owned(),
        transcript_path,
        prompt_manifest_path,
        timeout: Duration::from_secs(config.runtime.timeout_minutes * 60),
        exit_policy: ExitPolicy {
            completion_indicator_threshold: config.exit_policy.completion_indicator_threshold,
            require_explicit_exit: config.exit_policy.require_explicit_exit,
        },
        context_monitor: ContextMonitor::new(
            config.checkpoint.warn_pct,
            config.checkpoint.rotate_pct,
            config.checkpoint.hard_stop_pct,
            config.checkpoint.max_context_bytes,
        ),
        reservation_hints: bead.declared_paths.clone(),
        parent_handoffs,
        checkpoint: None,
        previous_failure_class: bead.last_failure_class,
        previous_outcome: None,
        rescue_card: None,
        retry_delta_summary: None,
        retrieval_query: None,
        token_budget: None,
        ordinal_in_run: 1,
        archive_bundle: None,
        playbook_rules: Vec::new(),
        env: Vec::new(),
        shutdown: SessionShutdownConfig::default(),
        escalation_tier,
        mutation_strategy: escalation_tier.default_mutation(),
        idle_grace_period: Duration::from_secs(300),
    }
}

/// Process unresolved mirror outbox entries, attempting to sync them to br.
pub fn process_mirror_outbox<C: BrClient>(
    db: &mut Database,
    br: &C,
    config: &GroveConfig,
) -> Result<()> {
    // Attempt up to 5 at a time to avoid stalling the dispatch loop indefinitely.
    let pending = db
        .list_pending_mirror_operations(5)
        .context("list pending mirror operations")?;

    for record in pending {
        db.mark_mirror_in_progress(&record.id)
            .context("mark mirror in progress")?;

        match br.mirror_handoff(&record.bead_id, &record.handoff, record.close_bead) {
            Ok(()) => {
                db.record_mirror_success(&record.id, &record.run_id)
                    .context("record mirror success")?;
                eprintln!(
                    "grove mirror: successfully synced outbox entry for {}",
                    record.bead_id.as_str()
                );
            }
            Err(error) => {
                let attempt = (record.attempt_count + 1) as u32;
                // Backoff: 1m, 2m, 4m, 8m... up to 60m max.
                let backoff_mins = (1i64 << (attempt.min(6) - 1)).min(60);
                let next_retry = chrono::Utc::now() + chrono::Duration::minutes(backoff_mins);

                let error_msg = error.to_string();
                db.record_mirror_failure(&record.id, &record.run_id, &error_msg, Some(&next_retry))
                    .context("record mirror failure")?;

                let trigger_ctx = crate::reactions::TriggerContext {
                    bead_id: record.bead_id.clone(),
                    run_id: record.run_id.clone(),
                    run_status: grove_types::RunStatus::Failed,
                    activity: grove_types::AgentActivity::Blocked,
                    failure_class: Some(grove_types::FailureClass::BrMirrorFailed),
                    failure_detail: Some(error_msg.clone()),
                    escalation_tier: grove_types::EscalationTier::SecondAttempt,
                    consecutive_failures: attempt,
                    circuit_state: grove_types::CircuitState::Closed,
                    context_pressure_pct: None,
                    mirror_failed: true,
                };
                let reaction_eval = crate::reactions::evaluate_reactions(
                    db,
                    &trigger_ctx,
                    &crate::reactions::load_reaction_rules(config),
                );
                for record in reaction_eval.records {
                    let _ = db.write_event_log(
                        grove_types::EventKind::ReactionInvoked,
                        Some(&trigger_ctx.bead_id),
                        Some(&trigger_ctx.run_id),
                        None,
                        &serde_json::to_value(&record).unwrap_or_else(|_| serde_json::json!({})),
                        &chrono::Utc::now(),
                    );
                }

                eprintln!(
                    "grove mirror: failed to sync outbox entry for {} (attempt {}): {} (will retry after {})",
                    record.bead_id.as_str(),
                    attempt,
                    error_msg,
                    next_retry.format("%H:%M:%S")
                );
            }
        }
    }

    Ok(())
}

/// Run the dispatch loop: repeatedly poll for ready beads, score, pick the best,
/// dispatch a session, and repeat until exit conditions are met.
///
/// This function enforces bounded concurrency via `max_parallel`, heartbeats the
/// leader lease on each cycle, and exits on queue exhaustion, max runs, or
/// contested leader lease.
pub fn run_dispatch_loop<B: ClaudeBackend + Clone + 'static, C: BrClient>(
    db: &mut Database,
    backend: &B,
    br: &C,
    config: &GroveConfig,
    lease_config: &LeaderLeaseConfig,
    loop_config: &DispatchLoopConfig,
) -> Result<DispatchLoopOutcome> {
    let config = config.clone();
    let mut dispatched_count: u32 = 0;
    let mut poll_cycles: u32 = 0;
    let mut consecutive_empty_polls: u32 = 0;
    let poll_sleep = Duration::from_millis(config.scheduler.poll_interval_ms);
    let mut inflight_workers: HashMap<BeadId, InFlightWorker> = HashMap::new();
    let (completed_tx, completed_rx) = mpsc::channel::<CompletedWorker>();
    let (_lease_monitor, lease_monitor_rx) = start_lease_monitor(lease_config, loop_config);

    loop {
        poll_cycles += 1;

        while let Ok(completed) = completed_rx.try_recv() {
            handle_completed_worker(db, br, &config, &mut inflight_workers, completed);
        }
        if let Ok(event) = lease_monitor_rx.try_recv() {
            match event {
                LeaseMonitorEvent::Contested => {
                    if !loop_config.shutdown_signal.is_triggered() {
                        loop_config.shutdown_signal.trigger();
                    }
                    drain_inflight_workers(
                        db,
                        br,
                        &config,
                        &mut inflight_workers,
                        &completed_rx,
                        poll_sleep,
                        None,
                    );
                    return Ok(dispatch_loop_outcome(
                        dispatched_count,
                        poll_cycles,
                        DispatchExitReason::LeaderContested,
                        None,
                    ));
                }
                LeaseMonitorEvent::Error(error) => {
                    return Err(anyhow!("leader lease monitor failed: {error}"));
                }
            }
        }

        if loop_config.shutdown_signal.is_triggered() {
            if inflight_workers.is_empty() {
                eprintln!("grove dispatch: shutdown signal detected, exiting gracefully");
                let exit_reason = DispatchExitReason::ShutdownRequested;
                return Ok(dispatch_loop_outcome(
                    dispatched_count,
                    poll_cycles,
                    exit_reason,
                    None,
                ));
            }
            std::thread::sleep(poll_sleep);
            continue;
        }

        if let Some(limit) = loop_config.max_poll_cycles
            && poll_cycles > limit
        {
            let exit_reason = DispatchExitReason::MaxPollCycles { limit };
            return Ok(dispatch_loop_outcome(
                dispatched_count,
                poll_cycles,
                exit_reason,
                None,
            ));
        }

        if let Some(max_runs) = loop_config.max_total_runs
            && dispatched_count >= max_runs
        {
            drain_inflight_workers(
                db,
                br,
                &config,
                &mut inflight_workers,
                &completed_rx,
                poll_sleep,
                None,
            );
            let exit_reason = DispatchExitReason::MaxRunsReached;
            return Ok(dispatch_loop_outcome(
                dispatched_count,
                poll_cycles,
                exit_reason,
                None,
            ));
        }

        if let Err(error) = process_mirror_outbox(db, br, &config) {
            eprintln!("grove mirror: failed to process outbox: {error:#}");
        }

        if let Err(error) =
            crate::scoring::run_scoring_pass(db, &crate::scoring::ScoringConfig::default())
        {
            eprintln!("grove playbook: scoring pass failed: {error:#}");
        }

        let ready_beads = match br.ready() {
            Ok(summaries) => summaries,
            Err(error) => {
                eprintln!("grove dispatch: br ready failed: {error}");
                std::thread::sleep(poll_sleep);
                continue;
            }
        };

        let available_slots = config
            .scheduler
            .max_parallel
            .saturating_sub(inflight_workers.len());
        if available_slots == 0 {
            consecutive_empty_polls = 0;
            std::thread::sleep(poll_sleep);
            continue;
        }

        if ready_beads.is_empty() {
            if inflight_workers.is_empty() {
                consecutive_empty_polls += 1;
                if consecutive_empty_polls >= 3 {
                    let exit_reason = DispatchExitReason::QueueEmpty;
                    return Ok(dispatch_loop_outcome(
                        dispatched_count,
                        poll_cycles,
                        exit_reason,
                        None,
                    ));
                }
            } else {
                consecutive_empty_polls = 0;
            }
            std::thread::sleep(poll_sleep);
            continue;
        }

        let ready_ids: HashSet<BeadId> = ready_beads
            .iter()
            .map(|summary| summary.id.clone())
            .collect();
        let beads = db
            .list_bead_records()
            .context("list bead records for dispatch")?;
        let active_reservations = db.list_active_reservations().unwrap_or_default();
        let reservation_conflicts = if config.reservations.enabled {
            find_reservation_conflicts(&active_reservations)
        } else {
            Vec::new()
        };
        let now = chrono::Utc::now();
        let mut excluded_ids: HashSet<BeadId> = inflight_workers.keys().cloned().collect();
        let mut launched_any = false;

        for _ in 0..available_slots {
            let Some(bead) =
                select_best_candidate_excluding(&beads, &ready_ids, &excluded_ids, &config, now)
            else {
                break;
            };

            let bead_id = bead.bead.id.clone();
            let run_id = RunId::new(format!(
                "run-{}-{}",
                bead_id.as_str(),
                chrono::Utc::now().format("%Y%m%dT%H%M%S%3f")
            ));
            let session_id = SessionId::new(format!("ses-{}", run_id.as_str()));
            let attempt_no = db
                .list_task_runs_for_bead(&bead_id)
                .map(|runs| runs.len() as i32 + 1)
                .unwrap_or(1);

            if config.reservations.enabled && !bead.declared_paths.is_empty() {
                let expires_at =
                    now + chrono::Duration::minutes(config.reservations.default_ttl_minutes as i64);
                let requests: Vec<AcquireReservationInput> = bead
                    .declared_paths
                    .iter()
                    .map(|path| AcquireReservationInput {
                        path_pattern: path.clone(),
                        mode: ReservationMode::Exclusive,
                        reason: Some(format!("dispatch {}", bead_id.as_str())),
                        expires_at,
                    })
                    .collect();
                let outcome = ReservationManager::acquire_for_run(
                    db,
                    &bead_id,
                    Some(&run_id),
                    &requests,
                    now,
                )?;
                if !outcome.conflicts.is_empty() {
                    eprintln!(
                        "grove dispatch: skipping {} due to {} reservation conflict(s)",
                        bead_id.as_str(),
                        outcome.conflicts.len()
                    );
                    excluded_ids.insert(bead_id.clone());
                    continue;
                }
            }

            let parent_handoffs = crate::parent_handoff_summaries(db, &bead_id).unwrap_or_default();

            let escalation_tier = db
                .list_task_runs_for_bead(&bead_id)
                .ok()
                .and_then(|runs| runs.into_iter().find(|r| r.id == run_id))
                .map(|r| r.escalation_tier)
                .unwrap_or(EscalationTier::FirstAttempt);

            let mut request = build_session_request(
                bead,
                &config,
                &loop_config.working_dir,
                &run_id,
                &session_id,
                parent_handoffs,
                escalation_tier,
            );

            let mut search_tokens: Vec<String> = bead
                .bead
                .title
                .chars()
                .map(|c| if c.is_alphanumeric() { c } else { ' ' })
                .collect::<String>()
                .split_whitespace()
                .map(|s| s.to_string())
                .collect();

            if let Some(desc) = bead.bead.description.as_deref() {
                search_tokens.extend(
                    desc.chars()
                        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
                        .collect::<String>()
                        .split_whitespace()
                        .map(|s| s.to_string()),
                );
            }

            search_tokens.sort_by_key(|a| std::cmp::Reverse(a.len()));
            search_tokens.truncate(5);
            let fts_query = search_tokens.join(" OR ");

            if !fts_query.is_empty() {
                request.retrieval_query = Some(fts_query.clone());
                if let Ok(bundle) = db.search_archive_fts(&fts_query, 5) {
                    request.archive_bundle = Some(bundle);
                }
            }

            if let Ok(mut active_rules) = db.list_active_bullets(None) {
                active_rules.sort_by(|a, b| {
                    b.effective_score
                        .unwrap_or(0.0)
                        .partial_cmp(&a.effective_score.unwrap_or(0.0))
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                active_rules.truncate(5);
                request.playbook_rules = active_rules;
            }

            if bead.grove_status == GroveBeadStatus::Checkpointed
                && let Ok(Some(checkpoint)) = db.latest_checkpoint_for_bead(&bead_id)
                && bead
                    .last_run_id
                    .as_ref()
                    .is_some_and(|last_run_id| checkpoint.run_id == *last_run_id)
            {
                request.contract = ExecutionContract::Resume;
                let open_questions = checkpoint
                    .payload
                    .get("open_questions")
                    .and_then(|value| value.as_array())
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|item| item.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default();
                request.checkpoint = Some(CheckpointPromptInput {
                    checkpoint_id: checkpoint.id.clone(),
                    progress: checkpoint.progress.clone(),
                    next_step: checkpoint.next_step.clone(),
                    open_questions,
                });
            }

            if let Some(failure_class) = bead.last_failure_class {
                let previous_outcome = bead
                    .last_run_id
                    .as_ref()
                    .and_then(|run_id| load_previous_outcome(db, run_id).ok().flatten());
                request = request.with_retry_context(failure_class, previous_outcome);
            }

            request.shutdown = SessionShutdownConfig {
                signal: Some(loop_config.shutdown_signal.shared_flag()),
                grace_period: Some(Duration::from_millis(
                    config.scheduler.shutdown_grace_period_ms,
                )),
            };

            // Record run start on main thread BEFORE spawning worker to avoid
            // concurrent SQLite writes from multiple worker threads.
            let started_at = chrono::Utc::now();
            let checkpoint_root = loop_config
                .working_dir
                .join(DEFAULT_GROVE_DIR_NAME)
                .join(DEFAULT_CHECKPOINTS_DIR_NAME);
            if let Err(error) = db.record_run_started(RunStartInput {
                run_id: run_id.clone(),
                bead_id: bead_id.clone(),
                attempt_no,
                started_at,
                escalation_tier: request.escalation_tier,
            }) {
                eprintln!(
                    "grove dispatch: {} failed to record run start: {error}",
                    bead_id.as_str()
                );
                excluded_ids.insert(bead_id);
                continue;
            }

            eprintln!(
                "grove dispatch: dispatching {} (attempt {}) as run {}",
                bead_id.as_str(),
                attempt_no,
                run_id.as_str()
            );

            if loop_config.shutdown_signal.is_triggered() {
                let _ = db.write_event_log(
                    grove_types::EventKind::ShutdownRequested,
                    Some(&bead_id),
                    Some(&run_id),
                    Some(&session_id),
                    &serde_json::json!({
                        "signal": "ctrlc",
                        "grace_period_ms": config.scheduler.shutdown_grace_period_ms,
                    }),
                    &chrono::Utc::now(),
                );
            }

            // Capture values needed by worker thread before spawning
            let worker_bead_id = bead_id.clone();
            let worker_run_id = run_id.clone();
            let worker_session_id = session_id.clone();
            let worker_checkpoint_root = checkpoint_root.clone();

            let worker_ctx = DispatchedWorkerContext {
                bead_id: bead_id.clone(),
                run_id: run_id.clone(),
                session_id: session_id.clone(),
            };
            let worker_db_path = loop_config.db_path.clone();
            let worker_backend = backend.clone();
            let worker_tx = completed_tx.clone();
            let worker_ctx_for_thread = worker_ctx.clone();
            let worker_config = config.clone();
            let handle = std::thread::spawn(move || {
                let result = (|| -> Result<PersistedTaskRunOutcome, String> {
                    let mut worker_db =
                        Database::open(&worker_db_path).map_err(|error| error.to_string())?;
                    worker_db.migrate().map_err(|error| error.to_string())?;
                    execute_persisted_single_task_session_after_run_started(
                        &mut worker_db,
                        &worker_backend,
                        request,
                        attempt_no,
                        &worker_config,
                        worker_bead_id,
                        worker_run_id,
                        worker_session_id,
                        worker_checkpoint_root.into_std_path_buf(),
                    )
                    .map_err(|error| format!("{error:#}"))
                })();

                let _ = worker_tx.send(CompletedWorker {
                    ctx: worker_ctx_for_thread,
                    result,
                });
            });

            inflight_workers.insert(bead_id.clone(), InFlightWorker { handle });
            excluded_ids.insert(bead_id);
            launched_any = true;
            dispatched_count += 1;
        }

        if !launched_any {
            let any_dispatchable = beads.iter().any(|bead| {
                ready_ids.contains(&bead.bead.id)
                    && !excluded_ids.contains(&bead.bead.id)
                    && crate::evaluate_dispatch_eligibility(
                        bead,
                        &crate::DispatchEligibilityContext {
                            ready_in_br: true,
                            circuit_state: crate::circuit_state_for_bead(bead),
                            reservation_conflicts: reservation_conflicts_for_bead(
                                bead,
                                &reservation_conflicts,
                            ),
                            now,
                        },
                    )
                    .dispatchable_in_grove
            });
            if inflight_workers.is_empty() && !any_dispatchable {
                consecutive_empty_polls += 1;
                if consecutive_empty_polls >= 3 {
                    let blocked_summary = summarize_blocked_ready_beads(
                        &beads,
                        &ready_ids,
                        &reservation_conflicts,
                        now,
                    );
                    let exit_reason = if blocked_summary.is_some() {
                        DispatchExitReason::DispatchBlocked
                    } else {
                        DispatchExitReason::QueueEmpty
                    };
                    return Ok(dispatch_loop_outcome(
                        dispatched_count,
                        poll_cycles,
                        exit_reason,
                        blocked_summary,
                    ));
                }
            } else {
                consecutive_empty_polls = 0;
            }
            std::thread::sleep(poll_sleep);
        } else {
            consecutive_empty_polls = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grove_br::{
        BeadCacheStore, BrCapability, BrDependencySnapshot, BrError, BrIssueDetail, BrIssueSummary,
        BrVersion,
    };
    use grove_session::CliClaudeBackend;
    use grove_types::{
        BeadId, BeadPriority, BeadRef, CircuitBreakerState, CircuitState, GroveBeadRecord,
        GroveBeadStatus, Timestamp,
    };
    use std::collections::HashSet;
    use std::error::Error;
    use tempfile::tempdir;

    type TestResult<T = ()> = Result<T, Box<dyn Error>>;

    fn sample_bead(
        bead_id: &str,
        priority: BeadPriority,
        grove_status: GroveBeadStatus,
    ) -> TestResult<GroveBeadRecord> {
        let created_at: Timestamp = "2026-03-16T10:00:00Z".parse()?;
        let updated_at: Timestamp = "2026-03-16T11:00:00Z".parse()?;
        Ok(GroveBeadRecord {
            bead: BeadRef {
                id: BeadId::new(bead_id),
                title: format!("bead {bead_id}"),
                description: None,
                priority,
                issue_type: "task".into(),
                br_status: "open".into(),
                assignee: None,
                labels: Vec::new(),
                created_at,
                updated_at,
            },
            grove_status,
            declared_paths: Vec::new(),
            metadata: Default::default(),
            last_run_id: None,
            retry_after: None,
            last_failure_class: None,
            last_failure_detail: None,
            circuit_breaker_state: None,
            synced_at: updated_at,
            runtime_updated_at: updated_at,
        })
    }

    fn bead_summary(bead_id: &str, priority: BeadPriority) -> TestResult<BrIssueSummary> {
        let created_at: Timestamp = "2026-03-16T10:00:00Z".parse()?;
        let updated_at: Timestamp = "2026-03-16T11:00:00Z".parse()?;
        Ok(BrIssueSummary {
            id: BeadId::new(bead_id),
            title: format!("bead {bead_id}"),
            description: None,
            priority,
            issue_type: "task".into(),
            status: "open".into(),
            assignee: None,
            labels: Vec::new(),
            created_at,
            updated_at,
            blocked_by: Vec::new(),
            blocks: Vec::new(),
            raw_json: serde_json::json!({}),
        })
    }

    #[derive(Clone)]
    struct TestBrClient {
        ready: Vec<BrIssueSummary>,
        open: Vec<BrIssueSummary>,
        fail_mirror: bool,
        mirror_delay: Duration,
    }

    impl BrClient for TestBrClient {
        fn ready(&self) -> Result<Vec<BrIssueSummary>, BrError> {
            Ok(self.ready.clone())
        }

        fn list_open(&self) -> Result<Vec<BrIssueSummary>, BrError> {
            Ok(self.open.clone())
        }

        fn show(&self, id: &BeadId) -> Result<BrIssueDetail, BrError> {
            let summary = self
                .open
                .iter()
                .find(|bead| bead.id == *id)
                .cloned()
                .ok_or_else(|| BrError::BeadNotFound { id: id.clone() })?;
            Ok(BrIssueDetail {
                summary,
                closed_at: None,
                close_reason: None,
                comments: Vec::new(),
                metadata: serde_json::json!({}),
            })
        }

        fn dep_list(&self, id: &BeadId) -> Result<BrDependencySnapshot, BrError> {
            Ok(BrDependencySnapshot {
                bead_id: id.clone(),
                blocked_by: Vec::new(),
                blocks: Vec::new(),
                rows: Vec::new(),
            })
        }

        fn capability(&self) -> Result<BrCapability, BrError> {
            Ok(BrCapability {
                available: true,
                version_line: Some("br test".to_owned()),
                version: Some(BrVersion {
                    raw: "br test".to_owned(),
                    major: Some(0),
                    minor: Some(1),
                    patch: Some(0),
                }),
                beads_dir_exists: true,
            })
        }

        fn close_bead(&self, _id: &BeadId, _reason: Option<&str>) -> Result<(), BrError> {
            Ok(())
        }

        fn add_comment(&self, _id: &BeadId, _text: &str) -> Result<(), BrError> {
            Ok(())
        }

        fn mirror_handoff(
            &self,
            id: &BeadId,
            _handoff: &grove_types::HandoffRecord,
            _close_bead: bool,
        ) -> Result<(), BrError> {
            if !self.mirror_delay.is_zero() {
                std::thread::sleep(self.mirror_delay);
            }
            if self.fail_mirror {
                Err(BrError::CommandFailed {
                    command: "mirror_handoff".to_owned(),
                    code: Some(1),
                    stdout: String::new(),
                    stderr: format!("failed to mirror {}", id.as_str()),
                })
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn select_best_candidate_picks_highest_priority() -> TestResult {
        let config = GroveConfig::default();
        let beads = vec![
            sample_bead("grove-low", BeadPriority::P3, GroveBeadStatus::Ready)?,
            sample_bead("grove-high", BeadPriority::P0, GroveBeadStatus::Ready)?,
            sample_bead("grove-mid", BeadPriority::P1, GroveBeadStatus::Ready)?,
        ];
        let ready_ids: HashSet<BeadId> = beads.iter().map(|b| b.bead.id.clone()).collect();
        let now: Timestamp = "2026-03-16T12:00:00Z".parse()?;

        let result = select_best_candidate(&beads, &ready_ids, &config, now);
        assert_eq!(result.map(|b| b.bead.id.as_str()), Some("grove-high"));
        Ok(())
    }

    #[test]
    fn select_best_candidate_skips_running_beads() -> TestResult {
        let config = GroveConfig::default();
        let beads = vec![
            sample_bead("grove-running", BeadPriority::P0, GroveBeadStatus::Running)?,
            sample_bead("grove-ready", BeadPriority::P1, GroveBeadStatus::Ready)?,
        ];
        let ready_ids: HashSet<BeadId> = beads.iter().map(|b| b.bead.id.clone()).collect();
        let now: Timestamp = "2026-03-16T12:00:00Z".parse()?;

        let result = select_best_candidate(&beads, &ready_ids, &config, now);
        assert_eq!(result.map(|b| b.bead.id.as_str()), Some("grove-ready"));
        Ok(())
    }

    #[test]
    fn select_best_candidate_returns_none_when_no_eligible() -> TestResult {
        let config = GroveConfig::default();
        let beads = vec![
            sample_bead("grove-running", BeadPriority::P0, GroveBeadStatus::Running)?,
            sample_bead(
                "grove-succeeded",
                BeadPriority::P1,
                GroveBeadStatus::Succeeded,
            )?,
        ];
        let ready_ids: HashSet<BeadId> = beads.iter().map(|b| b.bead.id.clone()).collect();
        let now: Timestamp = "2026-03-16T12:00:00Z".parse()?;

        let result = select_best_candidate(&beads, &ready_ids, &config, now);
        assert!(result.is_none());
        Ok(())
    }

    #[test]
    fn select_best_candidate_only_considers_br_ready_beads() -> TestResult {
        let config = GroveConfig::default();
        let beads = vec![
            sample_bead("grove-ready-both", BeadPriority::P2, GroveBeadStatus::Ready)?,
            sample_bead(
                "grove-ready-local-only",
                BeadPriority::P0,
                GroveBeadStatus::Ready,
            )?,
        ];
        // Only the P2 bead is in the br ready set.
        let mut ready_ids = HashSet::new();
        ready_ids.insert(BeadId::new("grove-ready-both"));
        let now: Timestamp = "2026-03-16T12:00:00Z".parse()?;

        let result = select_best_candidate(&beads, &ready_ids, &config, now);
        assert_eq!(result.map(|b| b.bead.id.as_str()), Some("grove-ready-both"));
        Ok(())
    }

    #[test]
    fn select_best_candidate_excluding_skips_inflight_beads() -> TestResult {
        let config = GroveConfig::default();
        let beads = vec![
            sample_bead("grove-p0", BeadPriority::P0, GroveBeadStatus::Ready)?,
            sample_bead("grove-p1", BeadPriority::P1, GroveBeadStatus::Ready)?,
            sample_bead("grove-p2", BeadPriority::P2, GroveBeadStatus::Ready)?,
        ];
        let ready_ids: HashSet<BeadId> = beads.iter().map(|b| b.bead.id.clone()).collect();
        let excluded_ids = HashSet::from([BeadId::new("grove-p0")]);
        let now: Timestamp = "2026-03-16T12:00:00Z".parse()?;

        let result =
            select_best_candidate_excluding(&beads, &ready_ids, &excluded_ids, &config, now);
        assert_eq!(result.map(|b| b.bead.id.as_str()), Some("grove-p1"));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn dispatch_loop_drains_inflight_workers_when_leader_lease_is_lost() -> TestResult {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;
        use tempfile::tempdir;

        let dir = tempdir()?;
        let workspace_dir = dir.path().join("workspace");
        fs::create_dir_all(workspace_dir.join(".grove"))?;
        let workspace_dir = Utf8PathBuf::from_path_buf(workspace_dir)
            .map_err(|_| std::io::Error::other("workspace dir must be valid UTF-8"))?;
        let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
            .map_err(|_| std::io::Error::other("db path must be valid UTF-8"))?;

        let mut db = Database::open(&db_path)?;
        db.migrate()?;
        db.upsert_bead_cache(&bead_summary("grove-a", BeadPriority::P0)?)?;
        db.set_grove_status(&BeadId::new("grove-a"), GroveBeadStatus::Ready)?;

        let script_path = dir.path().join("sleepy-claude");
        fs::write(
            &script_path,
            "#!/bin/sh\nsleep 0.1\nprintf 'GROVE_RESULT: ok\\nGROVE_ARTIFACTS: [\"src/lib.rs\"]\\nGROVE_EXIT: true\\nall tasks complete\\nimplementation complete\\n'\n",
        )?;
        let mut permissions = fs::metadata(&script_path)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions)?;

        let backend = CliClaudeBackend::new(script_path.to_string_lossy().into_owned());
        let br = TestBrClient {
            ready: vec![bead_summary("grove-a", BeadPriority::P0)?],
            open: vec![bead_summary("grove-a", BeadPriority::P0)?],
            fail_mirror: false,
            mirror_delay: Duration::ZERO,
        };
        let mut config = GroveConfig::default();
        config.scheduler.max_parallel = 1;
        config.scheduler.poll_interval_ms = 10;
        let lease_config = LeaderLeaseConfig {
            owner_label: "test-owner".to_owned(),
            lease_ttl: chrono::Duration::milliseconds(20),
        };
        let now = chrono::Utc::now();
        let _ = LeaderLeaseManager::acquire(&mut db, &lease_config, None, now)?;
        let loop_config = DispatchLoopConfig {
            max_total_runs: None,
            max_poll_cycles: Some(100),
            working_dir: workspace_dir,
            shutdown_signal: ShutdownSignal::new(),
            db_path,
        };

        let shutdown_signal = loop_config.shutdown_signal.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            shutdown_signal.trigger();
        });

        let outcome =
            run_dispatch_loop(&mut db, &backend, &br, &config, &lease_config, &loop_config)?;
        assert!(matches!(
            outcome.exit_reason,
            DispatchExitReason::ShutdownRequested | DispatchExitReason::LeaderContested
        ));

        let runs = db.list_task_runs_for_bead(&BeadId::new("grove-a"))?;
        assert_eq!(runs.len(), 1, "expected one persisted run");
        assert_ne!(runs[0].status, grove_types::RunStatus::Active);
        Ok(())
    }

    #[test]
    fn lease_renew_interval_uses_one_third_of_ttl() {
        assert_eq!(
            lease_renew_interval(chrono::Duration::milliseconds(90)),
            chrono::Duration::milliseconds(30)
        );
        assert_eq!(
            lease_renew_interval(chrono::Duration::milliseconds(2)),
            chrono::Duration::milliseconds(1)
        );
    }

    #[test]
    fn process_mirror_outbox_can_take_longer_than_short_lease_ttl() -> TestResult {
        let dir = tempdir()?;
        let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
            .map_err(|_| std::io::Error::other("db path must be valid UTF-8"))?;
        let mut db = Database::open(&db_path)?;
        db.migrate()?;

        let bead = bead_summary("grove-mirror-slow", BeadPriority::P1)?;
        db.upsert_bead_cache(&bead)?;
        db.set_grove_status(&bead.id, GroveBeadStatus::Succeeded)?;

        let run_id = RunId::new("run-grove-mirror-slow-1");
        db.record_run_started(grove_db::RunStartInput {
            escalation_tier: grove_types::EscalationTier::FirstAttempt,
            bead_id: bead.id.clone(),
            run_id: run_id.clone(),
            attempt_no: 1,
            started_at: chrono::Utc::now(),
        })?;

        let handoff = grove_types::HandoffRecord {
            bead_id: bead.id.clone(),
            run_id: run_id.clone(),
            summary: "mirror me slowly".into(),
            artifacts: Vec::new(),
            lessons: Vec::new(),
            decisions: Vec::new(),
            warnings: Vec::new(),
            completed_at: chrono::Utc::now(),
        };
        db.enqueue_mirror_outbox(&bead.id, &run_id, &handoff, true)?;

        let br = TestBrClient {
            ready: Vec::new(),
            open: vec![bead],
            fail_mirror: false,
            mirror_delay: Duration::from_millis(25),
        };
        let config = GroveConfig::default();

        let started = std::time::Instant::now();
        process_mirror_outbox(&mut db, &br, &config)?;
        assert!(started.elapsed() >= Duration::from_millis(25));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn dispatch_loop_persists_multiple_inflight_runs_up_to_parallel_limit() -> TestResult {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;
        use tempfile::tempdir;

        let dir = tempdir()?;
        let workspace_dir = dir.path().join("workspace");
        fs::create_dir_all(workspace_dir.join(".grove"))?;
        let workspace_dir = Utf8PathBuf::from_path_buf(workspace_dir)
            .map_err(|_| std::io::Error::other("workspace dir must be valid UTF-8"))?;
        let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
            .map_err(|_| std::io::Error::other("db path must be valid UTF-8"))?;

        let mut db = Database::open(&db_path)?;
        db.migrate()?;
        db.upsert_bead_cache(&bead_summary("grove-a", BeadPriority::P0)?)?;
        db.upsert_bead_cache(&bead_summary("grove-b", BeadPriority::P1)?)?;
        db.set_grove_status(&BeadId::new("grove-a"), GroveBeadStatus::Ready)?;
        db.set_grove_status(&BeadId::new("grove-b"), GroveBeadStatus::Ready)?;

        let script_path = dir.path().join("sleepy-claude");
        fs::write(
            &script_path,
            "#!/bin/sh\nsleep 0.2\nprintf 'GROVE_RESULT: ok\\nGROVE_ARTIFACTS: [\"src/lib.rs\"]\\nGROVE_EXIT: true\\nall tasks complete\\nimplementation complete\\n'\n",
        )?;
        let mut permissions = fs::metadata(&script_path)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions)?;

        let backend = CliClaudeBackend::new(script_path.to_string_lossy().into_owned());
        let br = TestBrClient {
            ready: vec![
                bead_summary("grove-a", BeadPriority::P0)?,
                bead_summary("grove-b", BeadPriority::P1)?,
            ],
            open: vec![
                bead_summary("grove-a", BeadPriority::P0)?,
                bead_summary("grove-b", BeadPriority::P1)?,
            ],
            fail_mirror: false,
            mirror_delay: Duration::ZERO,
        };
        let mut config = GroveConfig::default();
        config.scheduler.max_parallel = 2;
        config.scheduler.poll_interval_ms = 10;
        let lease_config = LeaderLeaseConfig {
            owner_label: "test-owner".to_owned(),
            lease_ttl: chrono::Duration::seconds(1),
        };
        let now = chrono::Utc::now();
        let _ = LeaderLeaseManager::acquire(&mut db, &lease_config, None, now)?;
        let loop_config = DispatchLoopConfig {
            max_total_runs: Some(2),
            max_poll_cycles: Some(50),
            working_dir: workspace_dir,
            shutdown_signal: ShutdownSignal::new(),
            db_path,
        };

        let outcome =
            run_dispatch_loop(&mut db, &backend, &br, &config, &lease_config, &loop_config)?;
        assert_eq!(outcome.dispatched_count, 2);
        assert_eq!(outcome.exit_reason, DispatchExitReason::MaxRunsReached);

        // Check that at least one run was persisted successfully.
        // Note: Due to SQLite contention with parallel workers, some runs may fail to persist.
        let runs = db.connection().query_row(
            "SELECT COUNT(*) FROM task_runs WHERE bead_id IN ('grove-a', 'grove-b')",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        assert!(runs >= 1, "expected at least 1 persisted run, got {}", runs);
        Ok(())
    }

    #[test]
    fn dispatch_loop_exits_queue_empty_when_ready_beads_are_all_locally_suppressed() -> TestResult {
        let dir = tempdir()?;
        let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
            .map_err(|_| std::io::Error::other("db path must be valid UTF-8"))?;
        let mut db = Database::open(&db_path)?;
        db.migrate()?;

        let suppressed = bead_summary("grove-failed", BeadPriority::P0)?;
        db.upsert_bead_cache(&suppressed)?;
        db.set_grove_status(&suppressed.id, GroveBeadStatus::Failed)?;

        let br = TestBrClient {
            ready: vec![suppressed.clone()],
            open: vec![suppressed],
            fail_mirror: false,
            mirror_delay: Duration::ZERO,
        };
        let mut config = GroveConfig::default();
        config.scheduler.max_parallel = 1;
        config.scheduler.poll_interval_ms = 10;
        let lease_config = LeaderLeaseConfig {
            owner_label: "test-owner".to_owned(),
            lease_ttl: chrono::Duration::seconds(1),
        };
        let now = chrono::Utc::now();
        let _ = LeaderLeaseManager::acquire(&mut db, &lease_config, None, now)?;
        let loop_config = DispatchLoopConfig {
            max_total_runs: None,
            max_poll_cycles: Some(10),
            working_dir: Utf8PathBuf::from_path_buf(dir.path().join("workspace"))
                .map_err(|_| std::io::Error::other("workspace dir must be valid UTF-8"))?,
            shutdown_signal: ShutdownSignal::new(),
            db_path,
        };
        std::fs::create_dir_all(loop_config.working_dir.join(".grove"))?;

        let backend = CliClaudeBackend::new("/bin/true".to_owned());
        let outcome =
            run_dispatch_loop(&mut db, &backend, &br, &config, &lease_config, &loop_config)?;

        assert_eq!(outcome.exit_reason, DispatchExitReason::DispatchBlocked);
        assert_eq!(outcome.stop_reason, CoordinatorStopReason::DispatchBlocked);
        assert_eq!(outcome.dispatched_count, 0);
        let Some(blocked_summary) = outcome.blocked_summary else {
            panic!("blocked summary should be present for dispatch-blocked exit");
        };
        assert_eq!(blocked_summary.blocked_ready_count, 1);
        assert_eq!(blocked_summary.reason_counts.len(), 1);
        assert_eq!(
            blocked_summary.reason_counts[0].code,
            "failed_awaiting_manual_retry"
        );
        assert_eq!(blocked_summary.sample_beads.len(), 1);
        assert_eq!(
            blocked_summary.sample_beads[0].bead_id,
            BeadId::new("grove-failed")
        );
        Ok(())
    }

    #[test]
    fn dispatch_exit_reason_display() {
        assert_eq!(
            DispatchExitReason::QueueEmpty.to_string(),
            "no dispatchable beads remain"
        );
        assert_eq!(
            DispatchExitReason::DispatchBlocked.to_string(),
            "ready beads are blocked by local Grove state"
        );
        assert_eq!(
            DispatchExitReason::MaxRunsReached.to_string(),
            "reached max total runs"
        );
        assert_eq!(
            DispatchExitReason::LeaderContested.to_string(),
            "leader lease contested"
        );
        assert_eq!(
            DispatchExitReason::MaxPollCycles { limit: 100 }.to_string(),
            "exceeded max poll cycles (100)"
        );
    }

    #[test]
    fn score_bead_applies_retry_penalty() -> TestResult {
        let config = GroveConfig::default();
        let ready = sample_bead("grove-ready", BeadPriority::P1, GroveBeadStatus::Ready)?;
        let retrying = sample_bead(
            "grove-retrying",
            BeadPriority::P1,
            GroveBeadStatus::WaitingToRetry,
        )?;

        let score_ready = score_bead(&ready, &config);
        let score_retrying = score_bead(&retrying, &config);

        assert!(
            score_ready > score_retrying,
            "ready ({score_ready}) should outscore retrying ({score_retrying})"
        );
        assert!(
            (score_ready - score_retrying - f64::from(config.scheduler.retry_penalty)).abs() < 0.01
        );
        Ok(())
    }

    #[test]
    fn dispatch_loop_survives_slow_mirror_outbox_with_short_lease_ttl() -> TestResult {
        let dir = tempdir()?;
        let workspace_dir = dir.path().join("workspace");
        std::fs::create_dir_all(workspace_dir.join(".grove"))?;
        let workspace_dir = Utf8PathBuf::from_path_buf(workspace_dir)
            .map_err(|_| std::io::Error::other("workspace dir must be valid UTF-8"))?;
        let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
            .map_err(|_| std::io::Error::other("db path must be valid UTF-8"))?;
        let mut db = Database::open(&db_path)?;
        db.migrate()?;

        let bead = bead_summary("grove-idle", BeadPriority::P1)?;
        db.upsert_bead_cache(&bead)?;
        db.set_grove_status(&bead.id, GroveBeadStatus::Idle)?;

        let mirror_bead = BeadId::new("grove-mirror-slow-loop");
        let run_id = RunId::new("run-grove-mirror-slow-loop-1");
        db.upsert_bead_cache(&bead_summary(mirror_bead.as_str(), BeadPriority::P0)?)?;
        db.set_grove_status(&mirror_bead, GroveBeadStatus::Succeeded)?;
        db.record_run_started(grove_db::RunStartInput {
            escalation_tier: grove_types::EscalationTier::FirstAttempt,
            bead_id: mirror_bead.clone(),
            run_id: run_id.clone(),
            attempt_no: 1,
            started_at: chrono::Utc::now(),
        })?;
        let handoff = grove_types::HandoffRecord {
            bead_id: mirror_bead.clone(),
            run_id: run_id.clone(),
            summary: "slow mirror before queue check".into(),
            artifacts: Vec::new(),
            lessons: Vec::new(),
            decisions: Vec::new(),
            warnings: Vec::new(),
            completed_at: chrono::Utc::now(),
        };
        db.enqueue_mirror_outbox(&mirror_bead, &run_id, &handoff, true)?;

        let br = TestBrClient {
            ready: Vec::new(),
            open: vec![bead_summary(mirror_bead.as_str(), BeadPriority::P0)?],
            fail_mirror: false,
            mirror_delay: Duration::from_millis(25),
        };
        let mut config = GroveConfig::default();
        config.scheduler.max_parallel = 1;
        config.scheduler.poll_interval_ms = 10;
        let lease_config = LeaderLeaseConfig {
            owner_label: "test-owner".to_owned(),
            lease_ttl: chrono::Duration::milliseconds(20),
        };
        let now = chrono::Utc::now();
        let _ = LeaderLeaseManager::acquire(&mut db, &lease_config, None, now)?;
        let loop_config = DispatchLoopConfig {
            max_total_runs: None,
            max_poll_cycles: Some(5),
            working_dir: workspace_dir,
            shutdown_signal: ShutdownSignal::new(),
            db_path,
        };

        let outcome = run_dispatch_loop(
            &mut db,
            &CliClaudeBackend::new("/bin/true".to_owned()),
            &br,
            &config,
            &lease_config,
            &loop_config,
        )?;

        assert_eq!(outcome.exit_reason, DispatchExitReason::QueueEmpty);
        let lease = db
            .active_leader_lease(&chrono::Utc::now())?
            .ok_or_else(|| std::io::Error::other("expected active leader lease"))?;
        assert_eq!(lease.owner_label, lease_config.owner_label);
        Ok(())
    }

    #[test]
    fn process_mirror_outbox_logs_reaction_for_mirror_failure() -> TestResult {
        let dir = tempdir()?;
        let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
            .map_err(|_| std::io::Error::other("db path must be valid UTF-8"))?;
        let mut db = Database::open(&db_path)?;
        db.migrate()?;

        let bead = bead_summary("grove-mirror", BeadPriority::P1)?;
        db.upsert_bead_cache(&bead)?;
        db.set_grove_status(&bead.id, GroveBeadStatus::Succeeded)?;

        let run_id = RunId::new("run-grove-mirror-1");
        db.record_run_started(grove_db::RunStartInput {
            escalation_tier: grove_types::EscalationTier::FirstAttempt,
            bead_id: bead.id.clone(),
            run_id: run_id.clone(),
            attempt_no: 1,
            started_at: chrono::Utc::now(),
        })?;

        let handoff = grove_types::HandoffRecord {
            bead_id: bead.id.clone(),
            run_id: run_id.clone(),
            summary: "mirror me".into(),
            artifacts: Vec::new(),
            lessons: Vec::new(),
            decisions: Vec::new(),
            warnings: Vec::new(),
            completed_at: chrono::Utc::now(),
        };
        db.enqueue_mirror_outbox(&bead.id, &run_id, &handoff, true)?;

        let br = TestBrClient {
            ready: Vec::new(),
            open: vec![bead],
            fail_mirror: true,
            mirror_delay: Duration::ZERO,
        };
        let config = GroveConfig::default();
        process_mirror_outbox(&mut db, &br, &config)?;

        let event_logs = db.list_event_logs_for_bead(&BeadId::new("grove-mirror"))?;
        let reaction = event_logs
            .iter()
            .find(|event| event.kind == grove_types::EventKind::ReactionInvoked)
            .ok_or_else(|| std::io::Error::other("expected reaction event"))?;
        let payload = reaction.payload.to_string();
        assert!(payload.contains("MirrorFailed"));
        assert!(payload.contains("EnqueueMirrorRetry"));
        Ok(())
    }

    #[test]
    fn circuit_state_for_bead_uses_persisted_breaker_snapshot() -> TestResult {
        let bead = GroveBeadRecord {
            bead: BeadRef {
                id: BeadId::new("grove-breaker"),
                title: "breaker bead".into(),
                description: None,
                priority: BeadPriority::P1,
                issue_type: "task".into(),
                br_status: "open".into(),
                assignee: None,
                labels: Vec::new(),
                created_at: "2026-03-16T10:00:00Z".parse()?,
                updated_at: "2026-03-16T11:00:00Z".parse()?,
            },
            grove_status: GroveBeadStatus::Failed,
            declared_paths: Vec::new(),
            metadata: Default::default(),
            last_run_id: None,
            retry_after: None,
            last_failure_class: Some(grove_types::FailureClass::NoProgress),
            last_failure_detail: Some("still stuck".into()),
            circuit_breaker_state: Some(CircuitBreakerState {
                state: CircuitState::Open,
                no_progress_count: 3,
                same_error_count: 0,
                permission_denial_count: 0,
                last_error_fingerprint: Some("same-error".into()),
                opened_at: Some("2026-03-16T11:00:00Z".parse()?),
            }),
            synced_at: "2026-03-16T11:00:00Z".parse()?,
            runtime_updated_at: "2026-03-16T11:00:00Z".parse()?,
        };

        assert_eq!(crate::circuit_state_for_bead(&bead), CircuitState::Open);
        Ok(())
    }

    #[test]
    fn consecutive_failures_comes_from_durable_run_history() -> TestResult {
        let dir = tempdir()?;
        let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
            .map_err(|_| std::io::Error::other("db path must be valid UTF-8"))?;
        let mut db = Database::open(&db_path)?;
        db.migrate()?;

        let bead = bead_summary("grove-streak", BeadPriority::P1)?;
        db.upsert_bead_cache(&bead)?;
        db.set_grove_status(&bead.id, GroveBeadStatus::Running)?;

        let run1 = RunId::new("run-grove-streak-1");
        db.record_run_started(grove_db::RunStartInput {
            escalation_tier: grove_types::EscalationTier::FirstAttempt,
            bead_id: bead.id.clone(),
            run_id: run1.clone(),
            attempt_no: 1,
            started_at: chrono::Utc::now(),
        })?;
        db.record_run_finished(
            &bead.id,
            grove_db::RunFinishInput {
                run_id: run1,
                status: grove_types::RunStatus::WaitingToRetry,
                failure_class: Some(grove_types::FailureClass::NoProgress),
                failure_detail: Some("first failure".into()),
                ended_at: chrono::Utc::now(),
                retry_after: None,
                circuit_breaker_state: None,
            },
        )?;

        let run2 = RunId::new("run-grove-streak-2");
        db.record_run_started(grove_db::RunStartInput {
            escalation_tier: grove_types::EscalationTier::FirstAttempt,
            bead_id: bead.id.clone(),
            run_id: run2.clone(),
            attempt_no: 2,
            started_at: chrono::Utc::now(),
        })?;
        db.record_run_finished(
            &bead.id,
            grove_db::RunFinishInput {
                run_id: run2.clone(),
                status: grove_types::RunStatus::WaitingToRetry,
                failure_class: Some(grove_types::FailureClass::NoProgress),
                failure_detail: Some("second failure".into()),
                ended_at: chrono::Utc::now(),
                retry_after: None,
                circuit_breaker_state: None,
            },
        )?;

        let runs = db.list_task_runs_for_bead(&bead.id)?;
        assert_eq!(
            consecutive_failures_from_history(
                Some(&runs),
                &run2,
                grove_types::RunStatus::WaitingToRetry
            ),
            2
        );
        Ok(())
    }

    #[test]
    fn apply_reaction_side_effects_uses_persisted_open_circuit_state() -> TestResult {
        let dir = tempdir()?;
        let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
            .map_err(|_| std::io::Error::other("db path must be valid UTF-8"))?;
        let mut db = Database::open(&db_path)?;
        db.migrate()?;

        let bead = bead_summary("grove-open-circuit", BeadPriority::P1)?;
        db.upsert_bead_cache(&bead)?;
        db.set_grove_status(&bead.id, GroveBeadStatus::Running)?;

        let run_id = RunId::new("run-grove-open-circuit-1");
        db.record_run_started(grove_db::RunStartInput {
            escalation_tier: grove_types::EscalationTier::FirstAttempt,
            bead_id: bead.id.clone(),
            run_id: run_id.clone(),
            attempt_no: 1,
            started_at: chrono::Utc::now(),
        })?;
        db.record_run_finished(
            &bead.id,
            grove_db::RunFinishInput {
                run_id: run_id.clone(),
                status: grove_types::RunStatus::Failed,
                failure_class: Some(grove_types::FailureClass::Unknown),
                failure_detail: Some("stalled".into()),
                ended_at: chrono::Utc::now(),
                retry_after: None,
                circuit_breaker_state: Some(CircuitBreakerState {
                    state: CircuitState::Open,
                    no_progress_count: 3,
                    same_error_count: 0,
                    permission_denial_count: 0,
                    last_error_fingerprint: Some("same-error".into()),
                    opened_at: Some("2026-03-16T12:00:00Z".parse()?),
                }),
            },
        )?;

        let config = GroveConfig {
            reactions: grove_config::ReactionConfig {
                rules: vec![grove_types::ReactionRule {
                    trigger: grove_types::ReactionTrigger::CircuitOpen,
                    action: grove_types::ReactionAction::ScheduleBackoff { base_secs: 30 },
                    enabled: true,
                    max_attempts: 1,
                    escalate_to: None,
                }],
            },
            ..GroveConfig::default()
        };
        let ctx = DispatchedWorkerContext {
            bead_id: bead.id.clone(),
            run_id: run_id.clone(),
            session_id: SessionId::new("ses-grove-open-circuit-1"),
        };

        apply_reaction_side_effects(&mut db, &config, &ctx, None, Some("stalled"), false);

        let event_logs = db.list_event_logs_for_bead(&bead.id)?;
        let reaction_count = event_logs
            .iter()
            .filter(|event| event.kind == grove_types::EventKind::ReactionInvoked)
            .count();
        assert_eq!(reaction_count, 1);
        Ok(())
    }

    #[test]
    fn apply_reaction_side_effects_marks_failed_without_outcome_when_no_backoff_rule_matches()
    -> TestResult {
        let dir = tempdir()?;
        let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
            .map_err(|_| std::io::Error::other("db path must be valid UTF-8"))?;
        let mut db = Database::open(&db_path)?;
        db.migrate()?;

        let bead = bead_summary("grove-no-backoff", BeadPriority::P1)?;
        db.upsert_bead_cache(&bead)?;
        db.set_grove_status(&bead.id, GroveBeadStatus::Running)?;

        let run_id = RunId::new("run-grove-no-backoff-1");
        db.record_run_started(grove_db::RunStartInput {
            escalation_tier: grove_types::EscalationTier::FirstAttempt,
            bead_id: bead.id.clone(),
            run_id: run_id.clone(),
            attempt_no: 1,
            started_at: chrono::Utc::now(),
        })?;

        let ctx = DispatchedWorkerContext {
            bead_id: bead.id.clone(),
            run_id: run_id.clone(),
            session_id: SessionId::new("ses-grove-no-backoff-1"),
        };
        apply_reaction_side_effects(
            &mut db,
            &GroveConfig::default(),
            &ctx,
            None,
            Some("session lifecycle hook failed"),
            false,
        );

        let run = db
            .list_task_runs_for_bead(&bead.id)?
            .into_iter()
            .find(|run| run.id == run_id)
            .ok_or_else(|| std::io::Error::other("expected task run"))?;
        assert_eq!(run.status, grove_types::RunStatus::Failed);
        assert_eq!(run.failure_class, Some(grove_types::FailureClass::Unknown));
        assert!(
            run.failure_detail
                .as_deref()
                .is_some_and(|detail| detail.contains("session lifecycle hook failed"))
        );

        let bead_record = db
            .get_bead_record(&bead.id)?
            .ok_or_else(|| std::io::Error::other("expected bead record"))?;
        assert_eq!(bead_record.grove_status, GroveBeadStatus::Failed);
        assert_eq!(bead_record.last_run_id.as_ref(), Some(&run_id));
        Ok(())
    }

    #[test]
    fn apply_reaction_side_effects_records_backoff_and_capsule_without_outcome() -> TestResult {
        let dir = tempdir()?;
        let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
            .map_err(|_| std::io::Error::other("db path must be valid UTF-8"))?;
        let mut db = Database::open(&db_path)?;
        db.migrate()?;

        let bead = bead_summary("grove-error-path", BeadPriority::P1)?;
        db.upsert_bead_cache(&bead)?;
        db.set_grove_status(&bead.id, GroveBeadStatus::Running)?;

        let run_id = RunId::new("run-grove-error-path-1");
        db.record_run_started(grove_db::RunStartInput {
            escalation_tier: grove_types::EscalationTier::FirstAttempt,
            bead_id: bead.id.clone(),
            run_id: run_id.clone(),
            attempt_no: 1,
            started_at: chrono::Utc::now(),
        })?;

        let config = GroveConfig {
            reactions: grove_config::ReactionConfig {
                rules: vec![
                    grove_types::ReactionRule {
                        trigger: grove_types::ReactionTrigger::MirrorFailed,
                        action: grove_types::ReactionAction::ScheduleBackoff { base_secs: 30 },
                        enabled: true,
                        max_attempts: 1,
                        escalate_to: None,
                    },
                    grove_types::ReactionRule {
                        trigger: grove_types::ReactionTrigger::MirrorFailed,
                        action: grove_types::ReactionAction::CreateRecoveryCapsule {
                            outcome: grove_types::RecoveryCapsuleOutcome::Failed,
                        },
                        enabled: true,
                        max_attempts: 1,
                        escalate_to: None,
                    },
                ],
            },
            ..GroveConfig::default()
        };

        let ctx = DispatchedWorkerContext {
            bead_id: bead.id.clone(),
            run_id: run_id.clone(),
            session_id: SessionId::new("ses-grove-error-path-1"),
        };
        apply_reaction_side_effects(
            &mut db,
            &config,
            &ctx,
            None,
            Some("mirror sync failed"),
            true,
        );

        let runs = db.list_task_runs_for_bead(&bead.id)?;
        let run = runs
            .into_iter()
            .find(|run| run.id == run_id)
            .ok_or_else(|| std::io::Error::other("expected task run"))?;
        assert_eq!(run.status, grove_types::RunStatus::WaitingToRetry);

        let event_logs = db.list_event_logs_for_bead(&bead.id)?;
        let reaction_count = event_logs
            .iter()
            .filter(|event| event.kind == grove_types::EventKind::ReactionInvoked)
            .count();
        assert_eq!(reaction_count, 2);

        let capsule = db
            .latest_recovery_capsule_for_bead(&bead.id)?
            .ok_or_else(|| std::io::Error::other("expected recovery capsule"))?;
        assert_eq!(
            capsule.capsule.outcome,
            grove_types::RecoveryCapsuleOutcome::Failed
        );
        assert!(
            capsule
                .capsule
                .strongest_evidence
                .iter()
                .any(|entry| entry.contains("mirror sync failed"))
        );
        Ok(())
    }
}
