use grove_types::{BeadId, FailureClass, GroveBeadStatus, RuntimeProvider};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyVerdict {
    Allow,
    AllowWithEscalation,
    RequireConfirmation,
    Deny,
}

impl PolicyVerdict {
    #[must_use]
    pub fn permits_execution(self) -> bool {
        matches!(self, Self::Allow | Self::AllowWithEscalation)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyDecision {
    pub verdict: PolicyVerdict,
    pub reason: String,
}

impl PolicyDecision {
    #[must_use]
    pub fn allow(reason: impl Into<String>) -> Self {
        Self {
            verdict: PolicyVerdict::Allow,
            reason: reason.into(),
        }
    }

    #[must_use]
    pub fn allow_with_escalation(reason: impl Into<String>) -> Self {
        Self {
            verdict: PolicyVerdict::AllowWithEscalation,
            reason: reason.into(),
        }
    }

    #[must_use]
    pub fn require_confirmation(reason: impl Into<String>) -> Self {
        Self {
            verdict: PolicyVerdict::RequireConfirmation,
            reason: reason.into(),
        }
    }

    #[must_use]
    pub fn deny(reason: impl Into<String>) -> Self {
        Self {
            verdict: PolicyVerdict::Deny,
            reason: reason.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderAction {
    TaskSession,
    CleanupCompaction,
}

impl ProviderAction {
    #[must_use]
    pub fn summary(self) -> &'static str {
        match self {
            Self::TaskSession => "task session dispatch",
            Self::CleanupCompaction => "cleanup synthesis compaction",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionPolicyAction {
    RetryReset {
        bead_id: BeadId,
        status: GroveBeadStatus,
        failure_class: Option<FailureClass>,
        ready_in_br: bool,
    },
    WorkspaceClean {
        dry_run: bool,
        confirmed: bool,
    },
    ProviderLaunch {
        provider: RuntimeProvider,
        provider_bin: String,
        init_args: Vec<String>,
        action: ProviderAction,
    },
    GitMutation {
        command_summary: String,
        destructive: bool,
        confirmed: bool,
    },
    ReservationEscalation {
        requested_patterns: Vec<String>,
        conflicting_patterns: Vec<String>,
        exclusive: bool,
    },
}

#[must_use]
pub fn evaluate_execution_policy(action: &ExecutionPolicyAction) -> PolicyDecision {
    match action {
        ExecutionPolicyAction::RetryReset {
            bead_id,
            status,
            failure_class,
            ready_in_br,
        } => evaluate_retry_reset(bead_id, *status, *failure_class, *ready_in_br),
        ExecutionPolicyAction::WorkspaceClean { dry_run, confirmed } => {
            evaluate_workspace_clean(*dry_run, *confirmed)
        }
        ExecutionPolicyAction::ProviderLaunch {
            provider,
            provider_bin,
            init_args,
            action,
        } => evaluate_provider_launch(*provider, provider_bin, init_args, *action),
        ExecutionPolicyAction::GitMutation {
            command_summary,
            destructive,
            confirmed,
        } => evaluate_git_mutation(command_summary, *destructive, *confirmed),
        ExecutionPolicyAction::ReservationEscalation {
            requested_patterns,
            conflicting_patterns,
            exclusive,
        } => evaluate_reservation_escalation(requested_patterns, conflicting_patterns, *exclusive),
    }
}

fn evaluate_retry_reset(
    bead_id: &BeadId,
    status: GroveBeadStatus,
    failure_class: Option<FailureClass>,
    ready_in_br: bool,
) -> PolicyDecision {
    match status {
        GroveBeadStatus::Failed | GroveBeadStatus::Checkpointed => {
            let mut reason = format!(
                "retry reset allowed for bead {} because grove status {:?} is retryable",
                bead_id.as_str(),
                status
            );
            if let Some(failure_class) = failure_class {
                reason.push_str(&format!(" (last failure: {:?})", failure_class));
            }
            if !ready_in_br {
                reason.push_str("; br does not currently report the bead as ready, so Grove will only reset local retry state");
            }
            PolicyDecision::allow(reason)
        }
        other => PolicyDecision::deny(format!(
            "retry reset denied for bead {} because grove status {:?} is not retryable; only Failed or Checkpointed beads can be retried",
            bead_id.as_str(),
            other
        )),
    }
}

fn evaluate_workspace_clean(dry_run: bool, confirmed: bool) -> PolicyDecision {
    if dry_run {
        return PolicyDecision::allow(
            "workspace cleanup allowed because --dry-run only inspects cleanup candidates",
        );
    }

    if confirmed {
        return PolicyDecision::allow(
            "workspace cleanup allowed because the operator confirmed the mutating cleanup with --yes",
        );
    }

    PolicyDecision::require_confirmation(
        "`grove clean` is mutating and may delete artifacts or compact logs; re-run with `--yes` or inspect with `--dry-run`",
    )
}

fn evaluate_provider_launch(
    provider: RuntimeProvider,
    provider_bin: &str,
    init_args: &[String],
    action: ProviderAction,
) -> PolicyDecision {
    let risky_flags = risky_provider_flags(init_args);
    if risky_flags.is_empty() {
        return PolicyDecision::allow(format!(
            "provider launch allowed for {} via {} using {}",
            provider.as_str(),
            provider_bin,
            action.summary()
        ));
    }

    PolicyDecision::allow_with_escalation(format!(
        "provider launch for {} via {} is escalated because {} uses risky init args: {}",
        provider.as_str(),
        provider_bin,
        action.summary(),
        risky_flags.join(", ")
    ))
}

fn evaluate_git_mutation(
    command_summary: &str,
    destructive: bool,
    confirmed: bool,
) -> PolicyDecision {
    if !destructive {
        return PolicyDecision::allow(format!(
            "git mutation allowed because `{command_summary}` is classified as non-destructive"
        ));
    }

    if confirmed {
        return PolicyDecision::allow(format!(
            "git mutation allowed because `{command_summary}` was explicitly confirmed"
        ));
    }

    PolicyDecision::require_confirmation(format!(
        "git mutation `{command_summary}` is destructive and requires explicit operator confirmation"
    ))
}

fn evaluate_reservation_escalation(
    requested_patterns: &[String],
    conflicting_patterns: &[String],
    exclusive: bool,
) -> PolicyDecision {
    if !exclusive || conflicting_patterns.is_empty() {
        return PolicyDecision::allow(format!(
            "file reservation request allowed for {} pattern(s)",
            requested_patterns.len()
        ));
    }

    PolicyDecision::require_confirmation(format!(
        "exclusive reservation escalation requires operator review because {} conflicting pattern(s) were detected: {}",
        conflicting_patterns.len(),
        conflicting_patterns.join(", ")
    ))
}

fn risky_provider_flags(init_args: &[String]) -> Vec<String> {
    init_args
        .iter()
        .filter(|arg| {
            let normalized = arg.to_ascii_lowercase();
            normalized.contains("danger")
                || normalized.contains("skip-permission")
                || normalized.contains("skip_permission")
                || normalized.contains("skippermissions")
                || normalized.contains("yolo")
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        ExecutionPolicyAction, PolicyVerdict, ProviderAction, evaluate_execution_policy,
    };
    use grove_types::{BeadId, GroveBeadStatus, RuntimeProvider};

    #[test]
    fn dry_run_clean_is_allowed() {
        let decision = evaluate_execution_policy(&ExecutionPolicyAction::WorkspaceClean {
            dry_run: true,
            confirmed: false,
        });

        assert_eq!(decision.verdict, PolicyVerdict::Allow);
        assert!(decision.reason.contains("--dry-run"));
    }

    #[test]
    fn mutating_clean_requires_confirmation() {
        let decision = evaluate_execution_policy(&ExecutionPolicyAction::WorkspaceClean {
            dry_run: false,
            confirmed: false,
        });

        assert_eq!(decision.verdict, PolicyVerdict::RequireConfirmation);
        assert!(decision.reason.contains("--yes"));
    }

    #[test]
    fn retry_reset_allows_retryable_statuses() {
        let decision = evaluate_execution_policy(&ExecutionPolicyAction::RetryReset {
            bead_id: BeadId::new("grove-1af.2"),
            status: GroveBeadStatus::Checkpointed,
            failure_class: None,
            ready_in_br: false,
        });

        assert_eq!(decision.verdict, PolicyVerdict::Allow);
        assert!(decision.reason.contains("Checkpointed"));
        assert!(
            decision
                .reason
                .contains("does not currently report the bead as ready")
        );
    }

    #[test]
    fn retry_reset_denies_non_retryable_statuses() {
        let decision = evaluate_execution_policy(&ExecutionPolicyAction::RetryReset {
            bead_id: BeadId::new("grove-1af.2"),
            status: GroveBeadStatus::Ready,
            failure_class: None,
            ready_in_br: true,
        });

        assert_eq!(decision.verdict, PolicyVerdict::Deny);
        assert!(decision.reason.contains("not retryable"));
    }

    #[test]
    fn dangerous_provider_flags_raise_escalation() {
        let decision = evaluate_execution_policy(&ExecutionPolicyAction::ProviderLaunch {
            provider: RuntimeProvider::Claude,
            provider_bin: "claude".to_owned(),
            init_args: vec![
                "--print".to_owned(),
                "--dangerously-skip-permissions".to_owned(),
            ],
            action: ProviderAction::TaskSession,
        });

        assert_eq!(decision.verdict, PolicyVerdict::AllowWithEscalation);
        assert!(decision.reason.contains("risky init args"));
        assert!(decision.reason.contains("--dangerously-skip-permissions"));
    }
}
