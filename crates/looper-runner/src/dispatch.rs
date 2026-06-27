//! Dispatch decision engine — determines if/when a triaged issue should be
//! dispatched to the Planner or Worker.

use chrono::{DateTime, Utc};

use crate::types::{DispatchAction, DispatchConfig, DispatchMode};

/// Labels used by the dispatch system.
pub const DISPATCH_PLAN: &str = "dispatch/plan";
pub const DISPATCH_IMPLEMENT: &str = "dispatch/implement";

/// Parse a slash command from a comment body.
///
/// - Only commands in the configured list are recognised.
/// - Commands inside code fences (```, ~~~) or blockquotes (>) are ignored.
/// - Must appear at the start of a line (with optional leading whitespace).
/// - Command boundary = space, tab, or end-of-line.
pub fn parse_slash_command(body: &str, configured: &[String]) -> Option<String> {
    let mut in_code_block = false;

    for line in body.lines() {
        let trimmed = line.trim_start();

        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_code_block = !in_code_block;
            continue;
        }

        if in_code_block {
            continue;
        }

        // Skip blockquote lines
        if trimmed.starts_with('>') {
            continue;
        }

        // Skip blank/whitespace-only lines
        if trimmed.is_empty() {
            continue;
        }

        // Check for configured commands
        for cmd in configured {
            let cmd_bytes = cmd.as_bytes();
            let line_bytes = trimmed.as_bytes();

            if line_bytes.len() < cmd_bytes.len() {
                continue;
            }

            if &line_bytes[..cmd_bytes.len()] == cmd_bytes {
                // Verify boundary: end of line or whitespace after command
                if line_bytes.len() == cmd_bytes.len() || line_bytes[cmd_bytes.len()].is_ascii_whitespace() {
                    return Some(cmd.clone());
                }
            }
        }
    }

    None
}

/// Find exactly one dispatch label from a list of labels.
/// Returns `None` if zero or more than one dispatch/ label exists.
pub fn single_dispatch_label(labels: &[String]) -> Option<String> {
    let dispatch_labels: Vec<&str> = labels.iter().filter(|l| l.starts_with("dispatch/")).map(|l| l.as_str()).collect();

    if dispatch_labels.len() == 1 {
        Some(dispatch_labels[0].to_string())
    } else {
        None
    }
}

/// Return the trigger labels for a given dispatch label.
pub fn trigger_labels_for_dispatch(dispatch_label: &str, cfg: &DispatchConfig) -> Vec<String> {
    match dispatch_label {
        DISPATCH_PLAN => cfg.planner_trigger_labels.clone(),
        DISPATCH_IMPLEMENT => cfg.worker_trigger_labels.clone(),
        _ => vec![],
    }
}

/// Return labels from `want` that are not in `existing`.
pub fn missing_labels(existing: &[String], want: &[String]) -> Vec<String> {
    want.iter().filter(|w| !existing.contains(w)).cloned().collect()
}

/// Check whether this issue/needs a dependency gate before dispatching.
#[allow(clippy::too_many_arguments)]
pub fn needs_dependency_gate(
    slash_command: Option<&str>,
    has_triaged_label: bool,
    dispatch_label: Option<&str>,
    has_trigger_labels: bool,
    cfg: &DispatchConfig,
    is_autonomous: bool,
    past_delay: bool,
    has_unsatisfied_blockers: bool,
) -> bool {
    if is_autonomous {
        has_triaged_label
            && dispatch_label.is_some()
            && cfg.hold_label.as_ref().is_none_or(|_h| true)
            && past_delay
            && !has_trigger_labels
            && !has_unsatisfied_blockers
    } else {
        slash_command.is_some()
            && has_triaged_label
            && dispatch_label.is_some()
            && dispatch_label.is_some_and(|d| {
                let command = slash_command.unwrap_or("");
                (d == DISPATCH_PLAN && command == "/plan") || (d == DISPATCH_IMPLEMENT && command == "/implement")
            })
            && !has_trigger_labels
    }
}

/// Core dispatch decision.
///
/// - `labels`: current issue labels
/// - `comments`: list of (author, body, id) tuples for parsing slash commands
/// - `has_write_access`: predicate to check if an author has write access
/// - `triaged_at`: when the issue was triaged (for autonomous delay)
/// - `dependency_labels`: labels representing blocked-by relationships
/// - `now`: current time
/// - `cfg`: dispatch configuration
pub fn decide(
    labels: &[String],
    comments: &[(String, String, i64)], // (author, body, comment_id)
    has_write_access: &dyn Fn(&str) -> bool,
    triaged_at: Option<DateTime<Utc>>,
    dependency_labels: &[String],
    now: DateTime<Utc>,
    cfg: &DispatchConfig,
) -> DispatchAction {
    let has_triaged = labels.contains(&cfg.triaged_label);
    let dispatch_label = single_dispatch_label(labels);
    let has_hold = cfg.hold_label.as_ref().is_some_and(|h| labels.contains(h));

    match cfg.mode {
        DispatchMode::HumanGated => {
            // Scan comments newest-first for a slash command
            let mut cmd_result: Option<(String, &str, i64)> = None; // (cmd, author, comment_id)

            for (author, body, comment_id) in comments.iter().rev() {
                if let Some(cmd) = parse_slash_command(body, &cfg.slash_commands) {
                    let authorized = if cfg.allowed_users.is_empty() {
                        has_write_access(author)
                    } else {
                        cfg.allowed_users.contains(author) || has_write_access(author)
                    };

                    if authorized {
                        cmd_result = Some((cmd, author, *comment_id));
                        break;
                    }
                    // Non-authorized: continue scanning older comments
                }
            }

            let (cmd, author, comment_id) = match cmd_result {
                Some(c) => c,
                None => return DispatchAction::no_op(),
            };

            // Validate: must have triaged label
            if !has_triaged {
                return DispatchAction {
                    no_op: false,
                    trigger_labels: vec![],
                    assign_to: None,
                    reaction_comment_id: Some(comment_id),
                    reaction_content: Some("confused".to_string()),
                    failure_comment_body: Some(format!(
                        "Issue is not triaged yet (missing `{}` label). Please wait for triage.",
                        cfg.triaged_label
                    )),
                };
            }

            // Validate: must have exactly one dispatch label
            let d_label = match dispatch_label {
                Some(ref l) => l.clone(),
                None => {
                    return DispatchAction {
                        no_op: false,
                        trigger_labels: vec![],
                        assign_to: None,
                        reaction_comment_id: Some(comment_id),
                        reaction_content: Some("confused".to_string()),
                        failure_comment_body: Some(
                            "Ambiguous dispatch: expected exactly one `dispatch/plan` or `dispatch/implement` label."
                                .to_string(),
                        ),
                    };
                }
            };

            // Validate: slash command must match dispatch label
            let expected = match cmd.as_str() {
                "/plan" => DISPATCH_PLAN,
                "/implement" => DISPATCH_IMPLEMENT,
                _ => return DispatchAction::no_op(),
            };

            if d_label != expected {
                return DispatchAction {
                    no_op: false,
                    trigger_labels: vec![],
                    assign_to: None,
                    reaction_comment_id: Some(comment_id),
                    reaction_content: Some("confused".to_string()),
                    failure_comment_body: Some(format!(
                        "Slash command `{cmd}` does not match dispatch label `{d_label}`. \
                         Use `/{expected_cmd}` instead or update the dispatch label.",
                        expected_cmd = if d_label == DISPATCH_PLAN { "plan" } else { "implement" }
                    )),
                };
            }

            let trigger = trigger_labels_for_dispatch(&d_label, cfg);
            let missing = missing_labels(labels, &trigger);

            // If trigger labels already applied → already dispatched
            if missing.is_empty() {
                return DispatchAction {
                    no_op: true,
                    trigger_labels: vec![],
                    assign_to: None,
                    reaction_comment_id: Some(comment_id),
                    reaction_content: Some("+1".to_string()),
                    failure_comment_body: None,
                };
            }

            // Check dependency gate
            if !dependency_labels.is_empty() {
                return DispatchAction {
                    no_op: false,
                    trigger_labels: vec![],
                    assign_to: None,
                    reaction_comment_id: Some(comment_id),
                    reaction_content: Some("confused".to_string()),
                    failure_comment_body: Some(format!(
                        "Cannot dispatch — unsatisfied dependency blockers: {:?}",
                        dependency_labels
                    )),
                };
            }

            DispatchAction {
                no_op: false,
                trigger_labels: trigger,
                assign_to: cfg.assign_to.clone().or(Some(author.to_string())),
                reaction_comment_id: Some(comment_id),
                reaction_content: Some("+1".to_string()),
                failure_comment_body: None,
            }
        }

        DispatchMode::Autonomous => {
            // Must be triaged
            if !has_triaged {
                return DispatchAction::no_op();
            }

            // Must have exactly one dispatch label
            let d_label = match dispatch_label {
                Some(l) => l,
                None => return DispatchAction::no_op(),
            };

            // Hold label blocks dispatch
            if has_hold {
                return DispatchAction::no_op();
            }

            let trigger = trigger_labels_for_dispatch(&d_label, cfg);

            // Already dispatched
            if missing_labels(labels, &trigger).is_empty() {
                return DispatchAction::no_op();
            }

            // Past autonomous delay?
            let past_delay = triaged_at.is_some_and(|t| (now - t).num_seconds() >= cfg.autonomous_delay.num_seconds());

            if !past_delay {
                return DispatchAction::no_op();
            }

            // Unsatisfied dependency blockers?
            if !dependency_labels.is_empty() {
                return DispatchAction::no_op();
            }

            let assign_to = cfg.assign_to.clone();

            DispatchAction {
                no_op: false,
                trigger_labels: trigger,
                assign_to,
                reaction_comment_id: None,
                reaction_content: None,
                failure_comment_body: None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn always_authorized(_author: &str) -> bool {
        true
    }

    fn never_authorized(_author: &str) -> bool {
        false
    }

    #[test]
    fn test_parse_slash_command_plan() {
        let cfg = vec!["/plan".to_string(), "/implement".to_string()];
        assert_eq!(parse_slash_command("/plan", &cfg), Some("/plan".to_string()));
        assert_eq!(parse_slash_command("  /plan  ", &cfg), Some("/plan".to_string()));
        assert_eq!(parse_slash_command("/plan something", &cfg), Some("/plan".to_string()));
    }

    #[test]
    fn test_parse_slash_command_implement() {
        let cfg = vec!["/plan".to_string(), "/implement".to_string()];
        assert_eq!(parse_slash_command("/implement", &cfg), Some("/implement".to_string()));
    }

    #[test]
    fn test_parse_slash_command_in_code_block() {
        let cfg = vec!["/plan".to_string()];
        let body = "```\n/plan\n```";
        assert_eq!(parse_slash_command(body, &cfg), None);
    }

    #[test]
    fn test_parse_slash_command_in_blockquote() {
        let cfg = vec!["/plan".to_string()];
        let body = "> /plan";
        assert_eq!(parse_slash_command(body, &cfg), None);
    }

    #[test]
    fn test_parse_slash_command_not_configured() {
        let cfg = vec!["/plan".to_string()];
        assert_eq!(parse_slash_command("/apply", &cfg), None);
    }

    #[test]
    fn test_parse_slash_command_empty() {
        let cfg = vec!["/plan".to_string()];
        assert_eq!(parse_slash_command("", &cfg), None);
        assert_eq!(parse_slash_command("   ", &cfg), None);
    }

    #[test]
    fn test_single_dispatch_label_plan() {
        let labels = vec!["bug".into(), "dispatch/plan".into()];
        assert_eq!(single_dispatch_label(&labels), Some("dispatch/plan".into()));
    }

    #[test]
    fn test_single_dispatch_label_none() {
        let labels = vec!["bug".into()];
        assert_eq!(single_dispatch_label(&labels), None);
    }

    #[test]
    fn test_single_dispatch_label_multiple() {
        let labels = vec!["dispatch/plan".into(), "dispatch/implement".into()];
        assert_eq!(single_dispatch_label(&labels), None);
    }

    #[test]
    fn test_missing_labels_some() {
        let existing = vec!["a".into(), "b".into()];
        let want = vec!["b".into(), "c".into()];
        assert_eq!(missing_labels(&existing, &want), vec!["c".to_string()]);
    }

    #[test]
    fn test_missing_labels_none() {
        let existing = vec!["a".into(), "b".into()];
        let want = vec!["a".into()];
        assert!(missing_labels(&existing, &want).is_empty());
    }

    #[test]
    fn test_decide_human_gated_no_command() {
        let cfg = DispatchConfig::default();
        let action = decide(
            &["looper:triaged".into(), "dispatch/plan".into()],
            &[],
            &always_authorized,
            None,
            &[],
            Utc::now(),
            &cfg,
        );
        assert!(action.no_op);
    }

    #[test]
    fn test_decide_human_gated_success() {
        let cfg = DispatchConfig::default();
        let action = decide(
            &["looper:triaged".into(), "dispatch/plan".into()],
            &[("user".into(), "/plan".into(), 42)],
            &always_authorized,
            None,
            &[],
            Utc::now(),
            &cfg,
        );
        assert!(!action.no_op);
        assert_eq!(action.trigger_labels, vec!["looper:plan"]);
        assert_eq!(action.reaction_content, Some("+1".to_string()));
    }

    #[test]
    fn test_decide_human_gated_not_triaged() {
        let cfg = DispatchConfig::default();
        let action = decide(
            &["dispatch/plan".into()],
            &[("user".into(), "/plan".into(), 42)],
            &always_authorized,
            None,
            &[],
            Utc::now(),
            &cfg,
        );
        assert!(!action.no_op);
        assert_eq!(action.reaction_content, Some("confused".to_string()));
    }

    #[test]
    fn test_decide_human_gated_unauthorized() {
        let cfg = DispatchConfig { allowed_users: vec!["admin".into()], ..Default::default() };
        let action = decide(
            &["looper:triaged".into(), "dispatch/plan".into()],
            &[("random".into(), "/plan".into(), 42)],
            &never_authorized,
            None,
            &[],
            Utc::now(),
            &cfg,
        );
        assert!(action.no_op); // unauthorized → skip, continue scanning
    }

    #[test]
    fn test_decide_autonomous_past_delay() {
        let cfg = DispatchConfig {
            mode: DispatchMode::Autonomous,
            autonomous_delay: chrono::Duration::minutes(10),
            ..Default::default()
        };
        let triaged_at = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let now = triaged_at + chrono::Duration::minutes(15);
        let action = decide(
            &["looper:triaged".into(), "dispatch/plan".into()],
            &[],
            &always_authorized,
            Some(triaged_at),
            &[],
            now,
            &cfg,
        );
        assert!(!action.no_op);
        assert_eq!(action.trigger_labels, vec!["looper:plan"]);
    }

    #[test]
    fn test_decide_autonomous_before_delay() {
        let cfg = DispatchConfig {
            mode: DispatchMode::Autonomous,
            autonomous_delay: chrono::Duration::minutes(10),
            ..Default::default()
        };
        let triaged_at = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let now = triaged_at + chrono::Duration::minutes(5);
        let action = decide(
            &["looper:triaged".into(), "dispatch/plan".into()],
            &[],
            &always_authorized,
            Some(triaged_at),
            &[],
            now,
            &cfg,
        );
        assert!(action.no_op);
    }

    #[test]
    fn test_decide_autonomous_hold() {
        let cfg = DispatchConfig { mode: DispatchMode::Autonomous, ..Default::default() };
        let action = decide(
            &["looper:triaged".into(), "dispatch/plan".into(), "looper:hold".into()],
            &[],
            &always_authorized,
            Some(Utc::now()),
            &[],
            Utc::now(),
            &cfg,
        );
        assert!(action.no_op);
    }

    #[test]
    fn test_decide_autonomous_blocked() {
        let cfg = DispatchConfig {
            mode: DispatchMode::Autonomous,
            autonomous_delay: chrono::Duration::minutes(0),
            ..Default::default()
        };
        let action = decide(
            &["looper:triaged".into(), "dispatch/plan".into()],
            &[],
            &always_authorized,
            Some(Utc::now()),
            &["blocked-by:something".into()],
            Utc::now(),
            &cfg,
        );
        assert!(action.no_op);
    }
}
