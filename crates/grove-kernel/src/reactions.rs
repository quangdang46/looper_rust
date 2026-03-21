//! Reaction engine — evaluates triggers against run state and invokes autonomous actions.
//!
//! Wires grove_types::reaction types into the dispatch loop. Reactions are
//! evaluated after each dispatch outcome and produce logged, auditable records.

use chrono::Utc;
use grove_db::Database;
use grove_types::{
    AgentActivity, BeadId, EscalationTier, FailureClass, RunId, RunStatus, SessionOutcome,
    reaction::{
        ReactionAction, ReactionContextSnapshot, ReactionOutcome, ReactionRecord, ReactionRule,
        ReactionTrigger, default_reactions,
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
            Some(grove_types::StopReason::Kill) | Some(grove_types::StopReason::Crash) => AgentActivity::Exited,
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
    _db: &mut Database,
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

        let reaction_id = format!(
            "rxn-{}-{}-{}",
            ctx.bead_id.as_str(),
            ctx.run_id.as_str(),
            records.len()
        );

        // Apply the action (in this implementation, actions are logged but
        // the actual side-effects like rescue injection happen in the dispatch
        // loop which reads the reaction records).
        let (outcome, applied_action, escalated_to) = apply_action(
            &rule.action,
            rule.escalate_to.as_deref(),
            &tier,
        );

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
        }
        ReactionTrigger::NoProgress { iterations } => {
            ctx.consecutive_failures >= *iterations
                && ctx.failure_class == Some(FailureClass::NoProgress)
        }
        ReactionTrigger::AgentIdle { .. } => ctx.activity == AgentActivity::Idle,
        ReactionTrigger::ContextPressureHigh => {
            ctx.context_pressure_pct.unwrap_or(0.0) > 0.85
        }
        ReactionTrigger::MirrorFailed => {
            // Mirror failures are handled by the outbox retry; this trigger
            // would be set by the mirror outbox processor. Not matched here.
            false
        }
        ReactionTrigger::RetryBudgetExhausted => {
            ctx.escalation_tier.is_terminal()
        }
    }
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
        (ReactionOutcome::Escalated, fallback.clone(), Some(fallback.clone()))
    } else {
        (ReactionOutcome::Skipped, action.clone(), None)
    }
}

/// Load reaction rules. Currently returns defaults; in the future these
/// will be loaded from `grove.toml` configuration.
pub fn load_reaction_rules(_config: &grove_config::GroveConfig) -> Vec<ReactionRule> {
    default_reactions()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_ctx(status: RunStatus, failures: u32, tier: EscalationTier) -> TriggerContext {
        TriggerContext {
            bead_id: BeadId::new("test-1"),
            run_id: RunId::new("run-1"),
            run_status: status,
            activity: AgentActivity::Active,
            failure_class: Some(FailureClass::NoProgress),
            failure_detail: None,
            escalation_tier: tier,
            consecutive_failures: failures,
            circuit_state: grove_types::CircuitState::Closed,
            context_pressure_pct: None,
        }
    }

    #[test]
    fn no_reactions_on_success() {
        let rules = default_reactions();
        let ctx = make_ctx(RunStatus::Succeeded, 0, EscalationTier::FirstAttempt);
        // We can't call evaluate_reactions without a real DB, but we can
        // test trigger matching individually
        for rule in &rules {
            assert!(
                !trigger_matches(&rule.trigger, &ctx),
                "No trigger should fire on success"
            );
        }
    }

    #[test]
    fn no_progress_triggers_after_threshold() {
        let rules = default_reactions();
        let ctx = make_ctx(RunStatus::Failed, 3, EscalationTier::FirstAttempt);
        let no_progress_rule = rules
            .iter()
            .find(|r| matches!(r.trigger, ReactionTrigger::NoProgress { .. }))
            .unwrap();
        assert!(trigger_matches(&no_progress_rule.trigger, &ctx));
    }

    #[test]
    fn retry_budget_exhausted_at_give_up() {
        let ctx = make_ctx(RunStatus::Failed, 5, EscalationTier::GiveUp);
        assert!(trigger_matches(&ReactionTrigger::RetryBudgetExhausted, &ctx));
    }

    #[test]
    fn context_pressure_triggers_above_85() {
        let mut ctx = make_ctx(RunStatus::Active, 0, EscalationTier::FirstAttempt);
        ctx.context_pressure_pct = Some(0.90);
        assert!(trigger_matches(&ReactionTrigger::ContextPressureHigh, &ctx));
    }

    #[test]
    fn idle_trigger_matches_idle_activity() {
        let mut ctx = make_ctx(RunStatus::WaitingToRetry, 1, EscalationTier::SecondAttempt);
        ctx.activity = AgentActivity::Idle;
        assert!(trigger_matches(
            &ReactionTrigger::AgentIdle { duration_secs: 300 },
            &ctx
        ));
    }

    #[test]
    fn action_escalates_at_terminal_tier() {
        let action = ReactionAction::InjectRescue {
            prompt: "test".into(),
        };
        let (outcome, _, _) = apply_action(&action, None, &EscalationTier::GiveUp);
        assert_eq!(outcome, ReactionOutcome::Escalated);
    }

    #[test]
    fn infer_blocked_activity_from_permission_denial() {
        let outcome = SessionOutcome {
            session: grove_types::ClaudeSessionRecord {
                id: grove_types::SessionId::new("ses-1"),
                run_id: RunId::new("run-1"),
                external_session_id: None,
                ordinal_in_run: 1,
                status: grove_types::SessionStatus::PermissionDenied,
                started_at: "2026-03-21T00:00:00Z".parse().unwrap(),
                ended_at: Some("2026-03-21T00:01:00Z".parse().unwrap()),
                prompt_id: None,
                prompt_manifest_path: None,
                prompt_bytes: 0,
                estimated_input_tokens: 0,
                estimated_output_tokens: 0,
                exit_code: Some(1),
                stop_reason: Some(grove_types::StopReason::PermissionDenied),
                transcript_path: "trace.jsonl".into(),
            },
            protocol_events: Vec::new(),
            analysis: grove_types::IterationAnalysis {
                permission_denials: 1,
                ..Default::default()
            },
            terminal_class: grove_types::SessionTerminalClass::PermissionDenied,
            context_pressure_pct: None,
            context_pressure_level: grove_types::ContextPressureLevel::Ok,
            stdout_tail: Vec::new(),
            stderr_tail: vec!["permission denied".into()],
        };

        assert_eq!(infer_agent_activity(&outcome, RunStatus::Failed), AgentActivity::Blocked);
    }
}
