use std::collections::HashSet;
use std::time::Instant;

use crate::claim::claim_and_run;
use crate::types::{
    ClaimPhase, Context, CoordinatorDiscoveryInput, FixerDiscoveryInput, PlannerDiscoveryInput, ReviewerDiscoveryInput,
    TickSummary, WorkerDiscoveryInput,
};
use crate::Scheduler;
use looper_storage::record::QueueItemRecord;

pub fn execute_scheduler_tick(scheduler: &Scheduler, ctx: &Context) -> TickSummary {
    let started_at = Instant::now();
    let mut summary = TickSummary::default();

    let (claimed, available) = execute_claim_phase(scheduler, ctx, ClaimPhase::PreDiscovery, &HashSet::new());
    summary.total_claimed += claimed;
    summary.total_available = summary.total_available.max(available);

    let repos = scheduler.repos();
    let projects = match repos.projects.list() {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("failed to list projects during tick: {e}");
            summary.discovery_errors.push(format!("list_projects: {e}"));
            return summary;
        }
    };
    drop(repos);

    let mut discovered_runnable_ids: HashSet<String> = HashSet::new();

    for project in &projects {
        if ctx.is_cancelled() {
            break;
        }
        if project.archived {
            continue;
        }

        summary.projects_processed += 1;

        // Resolve the GitHub repo identifier for discovery.
        // `gh` requires owner/repo format. Canonical: metadata.repo, else
        // repo_path when it is already owner/name (shared with admit-work).
        let repo = match looper_service::resolve_project_repo(project) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    project_id = %project.id,
                    "skipping discovery: {e}"
                );
                summary.discovery_errors.push(format!("{}: {e}", project.id));
                continue;
            }
        };

        if scheduler.planner_discovery_enabled {
            if let Some(ref planner) = scheduler.handlers.planner {
                let input =
                    PlannerDiscoveryInput { project_id: project.id.clone(), repo: repo.clone(), snapshot: None };
                let result = planner.discover_issues(ctx, input);
                track_runnable_ids(&result.queue_items, &mut discovered_runnable_ids);
                let (claimed, _) =
                    execute_claim_phase(scheduler, ctx, ClaimPhase::PostPlannerDiscovery, &discovered_runnable_ids);
                summary.total_claimed += claimed;
            }
        }

        if scheduler.coordinator_enabled {
            if let Some(ref coordinator) = scheduler.handlers.coordinator {
                let input =
                    CoordinatorDiscoveryInput { project_id: project.id.clone(), repo: repo.clone(), snapshot: None };
                let _result = coordinator.discover_issues(ctx, input);
                let (claimed, _) =
                    execute_claim_phase(scheduler, ctx, ClaimPhase::PostCoordinatorDiscovery, &discovered_runnable_ids);
                summary.total_claimed += claimed;
            }
        }

        if scheduler.reviewer_discovery_enabled {
            if let Some(ref reviewer) = scheduler.handlers.reviewer {
                let input =
                    ReviewerDiscoveryInput { project_id: project.id.clone(), repo: repo.clone(), snapshot: None };
                let result = reviewer.discover_pull_requests(ctx, input);
                track_runnable_ids(&result.queue_items, &mut discovered_runnable_ids);
                let (claimed, _) =
                    execute_claim_phase(scheduler, ctx, ClaimPhase::PostReviewerDiscovery, &discovered_runnable_ids);
                summary.total_claimed += claimed;
            }
        }

        if scheduler.fixer_discovery_enabled {
            if let Some(ref fixer) = scheduler.handlers.fixer {
                let input = FixerDiscoveryInput { project_id: project.id.clone(), repo: repo.clone(), snapshot: None };
                let result = fixer.discover_pull_requests(ctx, input);
                track_runnable_ids(&result.queue_items, &mut discovered_runnable_ids);
                let (claimed, _) =
                    execute_claim_phase(scheduler, ctx, ClaimPhase::PostFixerDiscovery, &discovered_runnable_ids);
                summary.total_claimed += claimed;
            }
        }

        if scheduler.worker_discovery_enabled {
            if let Some(ref worker) = scheduler.handlers.worker {
                let input = WorkerDiscoveryInput { project_id: project.id.clone(), repo: repo.clone(), snapshot: None };
                let result = worker.discover_issues(ctx, input);
                track_runnable_ids(&result.queue_items, &mut discovered_runnable_ids);
                let (claimed, _) =
                    execute_claim_phase(scheduler, ctx, ClaimPhase::PostWorkerDiscovery, &discovered_runnable_ids);
                summary.total_claimed += claimed;
            }
        }
    }

    let (claimed, _) = execute_claim_phase(scheduler, ctx, ClaimPhase::PostDiscovery, &discovered_runnable_ids);
    summary.total_claimed += claimed;

    // Post-discovery middleware pipeline (quality gate → outcome → patrol).
    // Runs on all queue items that were enqueued during this tick.
    if !discovered_runnable_ids.is_empty() {
        let ids: Vec<String> = discovered_runnable_ids.iter().cloned().collect();
        tracing::info!("Middleware hook: {} discovered IDs", ids.len());
        let repos = scheduler.repos();
        let items: Vec<QueueItemRecord> =
            ids.iter().filter_map(|id| repos.queue.get_by_id(id).ok().flatten()).collect();
        drop(repos);

        if !items.is_empty() {
            tracing::info!("Middleware hook: running on {} items", items.len());
            looper_storage::middleware::run_post_discovery_middleware(&items, &scheduler.repos());
        } else {
            tracing::info!("Middleware hook: discovered IDs but NO items found in DB");
        }
    } else {
        tracing::info!("Middleware hook: no discovered runnable IDs — skipping");
    }

    summary.duration = started_at.elapsed();

    tracing::info!(
        projects = summary.projects_processed,
        claimed = summary.total_claimed,
        duration_ms = summary.duration.as_millis() as u64,
        "scheduler tick completed"
    );

    summary
}

pub fn execute_claim_phase(
    scheduler: &Scheduler,
    ctx: &Context,
    phase: ClaimPhase,
    discovered_runnable_ids: &HashSet<String>,
) -> (usize, usize) {
    let _guard = match scheduler.claim_mu.lock() {
        Ok(g) => g,
        Err(_) => {
            tracing::warn!(phase = phase.as_str(), "claim lock contention, skipping");
            return (0, 0);
        }
    };

    let start = Instant::now();
    let mut available = scheduler.compute_available_slots(ctx);

    if available == 0 {
        if let Some(ref reconcile) = scheduler.reconcile_stale_runs {
            match reconcile(ctx) {
                Ok(_) => {
                    available = scheduler.compute_available_slots(ctx);
                }
                Err(e) => {
                    tracing::error!("stale run reconciliation failed: {e}");
                }
            }
        }
    }

    let claimed_items = if available > 0 {
        let items = claim_and_run(scheduler, ctx, available);
        if scheduler.handlers.has_claim_handler() && !items.is_empty() && !discovered_runnable_ids.is_empty() {
            for item in &items {
                if discovered_runnable_ids.contains(&item.id) {
                    scheduler.trigger_tick();
                    break;
                }
            }
        }
        items
    } else {
        Vec::new()
    };

    let claimed = claimed_items.len();
    let duration_ms = start.elapsed().as_millis() as u64;

    if claimed > 0 {
        tracing::info!(phase = phase.as_str(), available, claimed, duration_ms, "claim phase completed");
    }

    (claimed, available)
}

fn track_runnable_ids(items: &[QueueItemRecord], ids: &mut HashSet<String>) {
    for item in items {
        ids.insert(item.id.clone());
    }
}
