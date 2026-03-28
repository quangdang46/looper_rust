
#![allow(clippy::unwrap_used)]
use super::*;

#[test]
fn reaction_trigger_serializes() {
    let trigger = ReactionTrigger::NoProgress { iterations: 3 };
    let json = serde_json::to_string(&trigger).unwrap();
    assert!(json.contains("NoProgress"));
}

#[test]
fn reaction_action_serializes() {
    let action = ReactionAction::InjectRescue {
        prompt: "test".into(),
    };
    let json = serde_json::to_string(&action).unwrap();
    assert!(json.contains("InjectRescue"));
}

#[test]
fn reaction_action_capsule_outcome_mapping() {
    assert_eq!(
        ReactionAction::CreateRecoveryCapsule {
            outcome: RecoveryCapsuleOutcome::Checkpointed,
        }
        .recovery_capsule_outcome(),
        Some(RecoveryCapsuleOutcome::Checkpointed)
    );
    assert_eq!(
        ReactionAction::GiveUp.recovery_capsule_outcome(),
        Some(RecoveryCapsuleOutcome::Failed)
    );
    assert_eq!(
        ReactionAction::ForceCheckpoint.recovery_capsule_outcome(),
        None
    );
}

#[test]
fn reaction_outcome_serializes() {
    for outcome in [
        ReactionOutcome::Applied,
        ReactionOutcome::Escalated,
        ReactionOutcome::Skipped,
        ReactionOutcome::Failed,
    ] {
        let _encoded = serde_json::to_string(&outcome).unwrap();
    }
}

#[test]
fn reaction_context_snapshot_defaults() {
    let snapshot = ReactionContextSnapshot::default();
    assert!(snapshot.run_status.is_none());
    assert!(snapshot.activity.is_none());
    assert!(snapshot.escalation_tier.is_none());
}

#[test]
fn mutation_strategy_all_variants_serialize() {
    for strategy in [
        MutationStrategy::NarrowClaimedPaths,
        MutationStrategy::DifferentArchiveSnippet,
        MutationStrategy::AlternativeBeadContract,
        MutationStrategy::ReduceContextWindow,
        MutationStrategy::SwitchModel,
    ] {
        let _encoded = serde_json::to_string(&strategy).unwrap();
    }
}

#[test]
fn default_reactions_are_valid() {
    let reactions = default_reactions();
    assert!(!reactions.is_empty());
    for rule in &reactions {
        assert!(rule.enabled);
        assert!(rule.max_attempts > 0);
    }
    assert!(reactions.iter().any(|rule| {
        matches!(
            rule.action,
            ReactionAction::CreateRecoveryCapsule {
                outcome: RecoveryCapsuleOutcome::Failed
            }
        )
    }));
}

#[test]
fn reaction_record_carries_capsule_and_context() {
    let capsule = RecoveryCapsule::from_parts(
        RecoveryCapsuleOutcome::Failed,
        Some(FailureClass::NoProgress),
        Some("looped without new evidence"),
        Some("investigated logs"),
        Some("switch to narrower path"),
        Some("verification-first"),
        Some("narrowed claimed paths"),
        &["crates/grove-kernel/src/runtime.rs".to_owned()],
    )
    .unwrap();
    let record = ReactionRecord {
        id: "rxn-1".into(),
        trigger: ReactionTrigger::RetryBudgetExhausted,
        action: ReactionAction::CreateRecoveryCapsule {
            outcome: RecoveryCapsuleOutcome::Failed,
        },
        outcome: ReactionOutcome::Applied,
        escalated_to: None,
        recovery_capsule: Some(capsule),
        context: Some(ReactionContextSnapshot {
            run_status: Some(RunStatus::WaitingToRetry),
            activity: Some(AgentActivity::Blocked),
            escalation_tier: Some(EscalationTier::FinalAttempt),
            failure_class: Some(FailureClass::NoProgress),
            failure_detail: Some("looped without new evidence".into()),
            checkpoint_progress: Some("investigated logs".into()),
            checkpoint_next_step: Some("switch to narrower path".into()),
            retry_delta_summary: Some("narrowed claimed paths".into()),
        }),
        invoked_at: "2026-03-20T00:00:00Z".parse().unwrap(),
        success: true,
        error: None,
        run_id: Some(RunId::new("run-1")),
    };
    let encoded = serde_json::to_string(&record).unwrap();
    assert!(encoded.contains("RetryBudgetExhausted"));
    assert!(encoded.contains("looped without new evidence"));
}
