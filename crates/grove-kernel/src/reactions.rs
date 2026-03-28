//! Reaction engine — evaluates triggers against run state and invokes autonomous actions.
//!
//! Wires grove_types::reaction types into the dispatch loop. Reactions are
//! evaluated after each dispatch outcome and produce logged, auditable records.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use chrono::Utc;
use grove_db::Database;
use grove_types::{
    AgentActivity, BeadId, EscalationTier, FailureClass, RunId, RunStatus, SessionOutcome,
    reaction::{
        ReactionAction, ReactionContextSnapshot, ReactionOutcome, ReactionRecord, ReactionRule,
        ReactionTrigger,
    },
};

/// Runtime state snapshot used to evaluate triggers.
#[derive(Debug, Clone)]
pub struct TriggerContext {
    pub bead_id: BeadId,
    pub run_id: RunId,
    pub run_status: RunStatus,
    pub activity: AgentActivity,
    pub failure_class: Option<FailureClass>,
    pub failure_detail: Option<String>,
    pub escalation_tier: EscalationTier,
    pub consecutive_failures: u32,
    pub circuit_state: grove_types::CircuitState,
    pub context_pressure_pct: Option<f32>,
    pub mirror_failed: bool,
}

#[must_use]
pub fn infer_agent_activity(outcome: &SessionOutcome, run_status: RunStatus) -> AgentActivity {
    if matches!(run_status, RunStatus::Succeeded | RunStatus::Checkpointed) {
        return AgentActivity::Ready;
    }
    if matches!(run_status, RunStatus::WaitingToRetry) {
        return AgentActivity::Idle;
    }
    if matches!(run_status, RunStatus::Failed) {
        return match outcome.session.stop_reason {
            Some(grove_types::StopReason::Kill) | Some(grove_types::StopReason::Crash) => {
                AgentActivity::Exited
            }
            Some(grove_types::StopReason::PermissionDenied) => AgentActivity::Blocked,
            _ if outcome.analysis.permission_denials > 0 => AgentActivity::Blocked,
            _ if outcome.analysis.repeated_error_fingerprint.is_some() => AgentActivity::Idle,
            _ => AgentActivity::Exited,
        };
    }
    AgentActivity::Active
}

/// Result of evaluating and applying reactions for a single dispatch outcome.
#[derive(Debug, Clone)]
pub struct ReactionEvalResult {
    /// Reactions that fired.
    pub records: Vec<ReactionRecord>,
    /// Updated escalation tier after reactions.
    pub new_tier: EscalationTier,
}

/// Evaluate all reaction rules against the current trigger context.
///
/// Returns a list of reaction records for logging and the updated escalation tier.
pub fn evaluate_reactions(
    db: &mut Database,
    ctx: &TriggerContext,
    rules: &[ReactionRule],
) -> ReactionEvalResult {
    let mut records = Vec::new();
    let mut tier = ctx.escalation_tier;

    for rule in rules {
        if !rule.enabled {
            continue;
        }

        if !trigger_matches(&rule.trigger, ctx) {
            continue;
        }

        if prior_attempt_count(db, ctx, &rule.trigger) >= rule.max_attempts {
            continue;
        }

        let reaction_id = format!(
            "rxn-{}-{}-{}",
            ctx.bead_id.as_str(),
            ctx.run_id.as_str(),
            records.len()
        );

        // Apply the action (in this implementation, actions are logged but
        // the actual side-effects like rescue injection happen in the dispatch
        // loop which reads the reaction records).
        let (outcome, applied_action, escalated_to) =
            apply_action(&rule.action, rule.escalate_to.as_deref(), &tier);

        let record = ReactionRecord {
            id: reaction_id,
            trigger: rule.trigger.clone(),
            action: applied_action.clone(),
            outcome,
            escalated_to,
            recovery_capsule: None,
            context: Some(ReactionContextSnapshot {
                run_status: Some(ctx.run_status),
                activity: Some(ctx.activity),
                escalation_tier: Some(tier),
                failure_class: ctx.failure_class,
                failure_detail: ctx.failure_detail.clone(),
                checkpoint_progress: None,
                checkpoint_next_step: None,
                retry_delta_summary: None,
            }),
            invoked_at: Utc::now(),
            success: outcome == ReactionOutcome::Applied || outcome == ReactionOutcome::Escalated,
            error: None,
            run_id: Some(ctx.run_id.clone()),
        };

        records.push(record);

        // Escalate the tier on failure-driven reactions
        if ctx.run_status == RunStatus::Failed || ctx.run_status == RunStatus::WaitingToRetry {
            tier = tier.escalate();
        }
    }

    ReactionEvalResult {
        records,
        new_tier: tier,
    }
}

/// Check whether a trigger matches the current context.
fn trigger_matches(trigger: &ReactionTrigger, ctx: &TriggerContext) -> bool {
    match trigger {
        ReactionTrigger::CircuitOpen => {
            ctx.circuit_state == grove_types::CircuitState::Open
                || ctx.failure_class == Some(FailureClass::CircuitOpen)
        }
        ReactionTrigger::NoProgress { iterations } => {
            ctx.consecutive_failures >= *iterations
                && ctx.failure_class == Some(FailureClass::NoProgress)
        }
        ReactionTrigger::AgentIdle { .. } => ctx.activity == AgentActivity::Idle,
        ReactionTrigger::ContextPressureHigh => ctx.context_pressure_pct.unwrap_or(0.0) >= 0.85,
        ReactionTrigger::MirrorFailed => {
            ctx.mirror_failed || ctx.failure_class == Some(FailureClass::BrMirrorFailed)
        }
        ReactionTrigger::RetryBudgetExhausted => ctx.escalation_tier.is_terminal(),
    }
}

fn prior_attempt_count(db: &Database, ctx: &TriggerContext, trigger: &ReactionTrigger) -> u32 {
    db.list_events_for_run(&ctx.run_id)
        .ok()
        .map(|events| {
            events
                .into_iter()
                .filter(|event| event.kind == grove_types::EventKind::ReactionInvoked)
                .filter_map(|event| serde_json::from_value::<ReactionRecord>(event.payload).ok())
                .filter(|record| &record.trigger == trigger)
                .count() as u32
        })
        .unwrap_or(0)
}

/// Apply a reaction action, potentially escalating if the primary action
/// doesn't apply at the current tier.
fn apply_action(
    action: &ReactionAction,
    escalate_to: Option<&ReactionAction>,
    tier: &EscalationTier,
) -> (ReactionOutcome, ReactionAction, Option<ReactionAction>) {
    // If we're at GiveUp tier, always produce the terminal action
    if tier.is_terminal() {
        let terminal = ReactionAction::GiveUp;
        return (ReactionOutcome::Escalated, terminal.clone(), Some(terminal));
    }

    // Try the primary action
    let can_apply = match action {
        ReactionAction::RetryWithMutation { .. } => {
            // Only apply mutation at tier 3+
            tier.tier_number() >= 2
        }
        _ => true,
    };

    if can_apply {
        (ReactionOutcome::Applied, action.clone(), None)
    } else if let Some(fallback) = escalate_to {
        (
            ReactionOutcome::Escalated,
            fallback.clone(),
            Some(fallback.clone()),
        )
    } else {
        (ReactionOutcome::Skipped, action.clone(), None)
    }
}

/// Load reaction rules from config, falling back to defaults through config defaults.
pub fn load_reaction_rules(config: &grove_config::GroveConfig) -> Vec<ReactionRule> {
    config.reactions.rules.clone()
}

#[cfg(test)]
mod tests;
