//! Triage engine: LLM-powered issue classification.
//!
//! Ported from Go `legacy/internal/coordinator/triage/triage.go` (340 LOC).
//!
//! When an issue gets labeled "looper:plan", the triage engine sends
//! the issue title + body + repo context to an LLM, parses the structured
//! JSON response, and produces a Decision (valid/out-of-scope/unclear)
//! with labels and a comment.

use regex::Regex;
use serde::Deserialize;
use std::collections::HashSet;

// ---------------------------------------------------------------------------
// Disposition
// ---------------------------------------------------------------------------

/// The triage outcome for an issue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// Issue describes valid, implementable work.
    Valid,
    /// Issue is outside the project's scope.
    OutOfScope,
    /// Issue is unclear — needs more information.
    Unclear,
}

impl std::fmt::Display for Disposition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Disposition::Valid => write!(f, "valid"),
            Disposition::OutOfScope => write!(f, "out-of-scope"),
            Disposition::Unclear => write!(f, "unclear"),
        }
    }
}

impl Disposition {
    fn from_str(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "valid" => Some(Disposition::Valid),
            "out-of-scope" => Some(Disposition::OutOfScope),
            "unclear" => Some(Disposition::Unclear),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A comment on an issue.
#[derive(Debug, Clone)]
pub struct Comment {
    pub id: i64,
    pub author: String,
    pub author_association: String,
    pub body: String,
    pub created_at: String,
    pub updated_at: String,
}

/// A timeline event (label changes, etc.).
#[derive(Debug, Clone)]
pub struct TimelineEvent {
    pub event: String,
    pub created_at: String,
    pub label: String,
}

/// An issue from GitHub to be triaged.
#[derive(Debug, Clone)]
pub struct Issue {
    pub number: i64,
    pub title: String,
    pub body: String,
    pub url: String,
    pub author: String,
    pub created_at: String,
    pub updated_at: String,
    pub labels: Vec<String>,
    pub comments: Vec<Comment>,
    pub timeline: Vec<TimelineEvent>,
}

/// Context about the repository where the issue lives.
#[derive(Debug, Clone, Default)]
pub struct RepoContext {
    pub repo: String,
    pub working_directory: String,
    pub paths: Vec<String>,
    pub symbols: Vec<String>,
}

/// Triage configuration.
#[derive(Debug, Clone)]
pub struct Config {
    pub triaged_label: String,
    pub max_issue_age_days: i32,
    pub max_per_tick: i32,
    pub out_of_scope_label: String,
    pub unclear_label: String,
    pub re_triage_on_author_reply: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            triaged_label: "looper:triaged".into(),
            max_issue_age_days: 30,
            max_per_tick: 5,
            out_of_scope_label: "looper:out-of-scope".into(),
            unclear_label: "looper:needs-human".into(),
            re_triage_on_author_reply: true,
        }
    }
}

/// Full input to the triage decision.
#[derive(Debug, Clone)]
pub struct Input {
    pub issue: Issue,
    pub repo_context: RepoContext,
    pub config: Config,
    pub now: chrono::DateTime<chrono::Utc>,
}

/// A request to the LLM.
#[derive(Debug, Clone)]
pub struct Request {
    pub prompt: String,
    pub working_directory: String,
}

/// The LLM interface used by triage.
pub trait LLM {
    fn complete(&self, req: Request) -> Result<String, String>;
}

/// The triage decision produced by the LLM.
#[derive(Debug, Clone)]
pub struct Decision {
    pub no_op: bool,
    pub disposition: Option<Disposition>,
    pub clear_label_patterns: Vec<String>,
    pub remove_labels: Vec<String>,
    pub apply_labels: Vec<String>,
    pub comment_body: String,
    pub mark_triaged: bool,
}

/// The raw JSON output from the LLM.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LlmOutput {
    disposition: String,
    comment: String,
    #[serde(default)]
    labels: LlmLabels,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct LlmLabels {
    #[serde(default)]
    kind: Vec<String>,
    #[serde(default)]
    area: Vec<String>,
    #[serde(default)]
    complexity: Vec<String>,
    #[serde(default)]
    dispatch: Vec<String>,
}

// ---------------------------------------------------------------------------
// Token extraction
// ---------------------------------------------------------------------------

lazy_static::lazy_static! {
    static ref TOKEN_PATTERN: Regex = Regex::new(r"[A-Za-z][A-Za-z0-9_./-]{2,}").unwrap();
}

/// Extract search tokens from an issue for repo context.
pub fn search_tokens(issue: &Issue) -> Vec<String> {
    let text = format!("{}\n{}", issue.title, issue.body);
    let mut seen = HashSet::new();
    let mut tokens: Vec<String> = Vec::new();

    for cap in TOKEN_PATTERN.find_iter(&text) {
        let trimmed: String = cap.as_str().chars().filter(|c| !c.is_whitespace()).collect();
        let mut m = trimmed.trim_matches(|c: char| c == '.' || c == '/' || c == '-').to_lowercase();
        if m.len() < 3 {
            continue;
        }
        // Trim trailing separators
        while m.ends_with(|c: char| c == '.' || c == '/' || c == '-') {
            m.pop();
        }
        if m.len() < 3 || seen.contains(&m) {
            continue;
        }
        seen.insert(m.clone());
        tokens.push(m);
        if tokens.len() >= 12 {
            break;
        }
    }
    tokens
}

// ---------------------------------------------------------------------------
// Allowed label functions
// ---------------------------------------------------------------------------

/// Allowed kind labels.
pub fn allowed_kinds() -> Vec<&'static str> {
    vec!["kind/bug", "kind/feature", "kind/docs", "kind/refactor"]
}

/// Allowed area labels.
pub fn allowed_areas() -> Vec<&'static str> {
    vec![
        "area/api",
        "area/config",
        "area/coordinator",
        "area/docs",
        "area/github",
        "area/runtime",
        "area/testing",
        "area/planner",
        "area/worker",
        "area/reviewer",
    ]
}

/// Allowed complexity labels.
pub fn allowed_complexities() -> Vec<&'static str> {
    vec!["complexity/s", "complexity/m", "complexity/l"]
}

/// Allowed dispatch labels.
pub fn allowed_dispatches() -> Vec<&'static str> {
    vec!["dispatch/plan", "dispatch/implement"]
}

/// All allowed labels across all categories.
pub fn allowed_label_universe() -> Vec<&'static str> {
    let mut labels: Vec<&'static str> = Vec::new();
    labels.extend(allowed_kinds());
    labels.extend(allowed_areas());
    labels.extend(allowed_complexities());
    labels.extend(allowed_dispatches());
    labels.sort();
    labels
}

// ---------------------------------------------------------------------------
// Decision functions
// ---------------------------------------------------------------------------

/// Create a no-op decision (triage skipped).
pub fn no_op_decision() -> Decision {
    Decision {
        no_op: true,
        disposition: None,
        clear_label_patterns: vec![],
        remove_labels: vec![],
        apply_labels: vec![],
        comment_body: String::new(),
        mark_triaged: false,
    }
}

/// Create a re-triage decision (remove unclear and triaged labels).
pub fn re_triage_decision(cfg: &Config) -> Decision {
    Decision {
        no_op: false,
        disposition: Some(Disposition::Unclear),
        clear_label_patterns: vec![],
        remove_labels: vec![cfg.unclear_label.clone(), cfg.triaged_label.clone()],
        apply_labels: vec![],
        comment_body: String::new(),
        mark_triaged: false,
    }
}

/// Decide whether an issue should be triaged.
pub fn should_triage(issue: &Issue, cfg: &Config, now: chrono::DateTime<chrono::Utc>) -> bool {
    if has_label(&issue.labels, &cfg.triaged_label) {
        return false;
    }
    match parse_time(&issue.created_at) {
        Some(created) => {
            let age = now.signed_duration_since(created);
            age.num_days() <= cfg.max_issue_age_days as i64
        }
        None => false,
    }
}

/// Decide whether an issue should be re-triaged after author reply.
pub fn should_re_triage(issue: &Issue, cfg: &Config, _now: chrono::DateTime<chrono::Utc>) -> bool {
    if !cfg.re_triage_on_author_reply || !has_label(&issue.labels, &cfg.unclear_label) {
        return false;
    }
    match needs_info_applied_at(issue, &cfg.unclear_label) {
        Some(needs_info_at) => {
            for comment in &issue.comments {
                if let Some(when) = parse_time(&comment.created_at) {
                    if comment.author == issue.author && when >= needs_info_at {
                        return true;
                    }
                }
            }
            false
        }
        None => false,
    }
}

/// Limit items per tick.
pub fn limit_per_tick<T: Clone>(items: &[T], max: i32) -> Vec<T> {
    if max <= 0 || items.len() <= max as usize {
        return items.to_vec();
    }
    items[..max as usize].to_vec()
}

/// Build the structured LLM prompt for triage.
pub fn build_prompt(input: &Input) -> String {
    let mut b = String::new();
    b.push_str("You are Looper Coordinator triage. Return strict JSON only.\n");
    b.push_str("Allowed dispositions: valid, out-of-scope, unclear.\n");
    b.push_str("Allowed kind labels: ");
    b.push_str(&allowed_kinds().join(", "));
    b.push_str("\nAllowed area labels: ");
    b.push_str(&allowed_areas().join(", "));
    b.push_str("\nAllowed complexity labels: ");
    b.push_str(&allowed_complexities().join(", "));
    b.push_str("\nAllowed dispatch labels: ");
    b.push_str(&allowed_dispatches().join(", "));
    b.push_str("\nOutput schema:\n");
    b.push_str(
        r#"{"disposition":"valid|out-of-scope|unclear","comment":"string","labels":{"kind":["kind/..."],"area":["area/..."],"complexity":["complexity/..."],"dispatch":["dispatch/..."]}}"#,
    );
    b.push_str("\n\nIssue:\n");
    b.push_str(&input.issue.title);
    b.push_str("\n\n");
    b.push_str(input.issue.body.trim());
    if !input.repo_context.paths.is_empty() {
        b.push_str("\n\nRelevant paths:\n- ");
        b.push_str(&input.repo_context.paths.join("\n- "));
    }
    if !input.repo_context.symbols.is_empty() {
        b.push_str("\n\nRelevant symbols:\n- ");
        b.push_str(&input.repo_context.symbols.join("\n- "));
    }
    b
}

/// Run the triage decision.
pub fn decide(llm: Option<&dyn LLM>, input: &Input) -> Decision {
    let llm = match llm {
        Some(l) => l,
        None => return no_op_decision(),
    };
    let req = Request { prompt: build_prompt(input), working_directory: input.repo_context.working_directory.clone() };
    let raw = match llm.complete(req) {
        Ok(r) => r,
        Err(_) => return no_op_decision(),
    };
    parse_decision(&raw, &input.config).unwrap_or_else(|_| no_op_decision())
}

// ---------------------------------------------------------------------------
// Decision parsing
// ---------------------------------------------------------------------------

fn parse_decision(raw: &str, cfg: &Config) -> Result<Decision, String> {
    let output: LlmOutput = serde_json::from_str(raw.trim()).map_err(|e| format!("JSON parse error: {e}"))?;

    let comment = output.comment.trim().to_string();
    if comment.is_empty() {
        return Err("comment is required".into());
    }

    let clear = vec![
        "kind/*",
        "area/*",
        "complexity/*",
        "dispatch/*",
        cfg.out_of_scope_label.as_str(),
        cfg.unclear_label.as_str(),
    ];
    let clear_strs: Vec<String> = clear.into_iter().map(|s| s.to_string()).collect();

    match Disposition::from_str(&output.disposition) {
        Some(Disposition::Valid) => {
            let kind = require_exactly_one(&output.labels.kind, &allowed_kinds())?;
            let area = require_exactly_one(&output.labels.area, &allowed_areas())?;
            let complexity = require_exactly_one(&output.labels.complexity, &allowed_complexities())?;
            let dispatch = require_exactly_one(&output.labels.dispatch, &allowed_dispatches())?;
            let mut apply = vec![kind, area, complexity, dispatch, cfg.triaged_label.clone()];
            apply.sort();
            apply.dedup();
            Ok(Decision {
                no_op: false,
                disposition: Some(Disposition::Valid),
                clear_label_patterns: clear_strs,
                remove_labels: vec![],
                apply_labels: apply,
                comment_body: comment,
                mark_triaged: true,
            })
        }
        Some(Disposition::OutOfScope) => {
            if has_any_labels(&output.labels) {
                return Err("unexpected labels for out-of-scope disposition".into());
            }
            let apply = vec![cfg.out_of_scope_label.clone(), cfg.triaged_label.clone()];
            Ok(Decision {
                no_op: false,
                disposition: Some(Disposition::OutOfScope),
                clear_label_patterns: clear_strs,
                remove_labels: vec![],
                apply_labels: apply,
                comment_body: comment,
                mark_triaged: true,
            })
        }
        Some(Disposition::Unclear) => {
            if has_any_labels(&output.labels) {
                return Err("unexpected labels for unclear disposition".into());
            }
            let apply = vec![cfg.unclear_label.clone(), cfg.triaged_label.clone()];
            Ok(Decision {
                no_op: false,
                disposition: Some(Disposition::Unclear),
                clear_label_patterns: clear_strs,
                remove_labels: vec![],
                apply_labels: apply,
                comment_body: comment,
                mark_triaged: true,
            })
        }
        None => Err(format!("unknown disposition: {}", output.disposition)),
    }
}

fn require_exactly_one(values: &[String], allowed: &[&str]) -> Result<String, String> {
    if values.len() != 1 {
        return Err(format!("expected exactly one value, got {}", values.len()));
    }
    let value = values[0].trim().to_string();
    let allowed_set: HashSet<&&str> = allowed.iter().collect();
    if !allowed_set.contains(&value.as_str()) {
        return Err(format!("unknown value \"{value}\""));
    }
    Ok(value)
}

fn has_any_labels(labels: &LlmLabels) -> bool {
    !labels.kind.is_empty() || !labels.area.is_empty() || !labels.complexity.is_empty() || !labels.dispatch.is_empty()
}

fn needs_info_applied_at(issue: &Issue, unclear_label: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    for event in issue.timeline.iter().rev() {
        if event.event == "labeled" && event.label == unclear_label {
            return parse_time(&event.created_at);
        }
    }
    None
}

fn parse_time(raw: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    // Try RFC3339 first, then ISO string
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(raw) {
        return Some(dt.with_timezone(&chrono::Utc));
    }
    if let Ok(dt) = chrono::DateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S%.3fZ") {
        return Some(dt.with_timezone(&chrono::Utc));
    }
    None
}

fn has_label(labels: &[String], want: &str) -> bool {
    labels.iter().any(|l| l == want)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    struct MockLlm(&'static str);
    impl LLM for MockLlm {
        fn complete(&self, _req: Request) -> Result<String, String> {
            Ok(self.0.to_string())
        }
    }

    fn sample_issue() -> Issue {
        Issue {
            number: 42,
            title: "Add login page".into(),
            body: "We need a login page with email/password.\n\nAcceptance:\n- [ ] Form with email + password fields\n- [ ] Submit button".into(),
            url: "https://github.com/owner/repo/issues/42".into(),
            author: "user1".into(),
            created_at: "2026-06-20T12:00:00Z".into(),
            updated_at: "2026-06-20T12:00:00Z".into(),
            labels: vec!["looper:plan".into()],
            comments: vec![],
            timeline: vec![],
        }
    }

    #[test]
    fn test_decide_valid() {
        let llm = MockLlm(
            r#"{"disposition":"valid","comment":"This is a valid feature request.","labels":{"kind":["kind/feature"],"area":["area/api"],"complexity":["complexity/m"],"dispatch":["dispatch/plan"]}}"#,
        );
        let input = Input {
            issue: sample_issue(),
            repo_context: RepoContext::default(),
            config: Config::default(),
            now: Utc.with_ymd_and_hms(2026, 6, 21, 12, 0, 0).unwrap(),
        };
        let d = decide(Some(&llm), &input);
        assert!(!d.no_op);
        assert_eq!(d.disposition, Some(Disposition::Valid));
        assert!(d.apply_labels.contains(&"kind/feature".to_string()));
        assert!(d.mark_triaged);
    }

    #[test]
    fn test_decide_out_of_scope() {
        let llm = MockLlm(r#"{"disposition":"out-of-scope","comment":"This is outside our scope.","labels":{}}"#);
        let input = Input {
            issue: sample_issue(),
            repo_context: RepoContext::default(),
            config: Config::default(),
            now: Utc.with_ymd_and_hms(2026, 6, 21, 12, 0, 0).unwrap(),
        };
        let d = decide(Some(&llm), &input);
        assert!(!d.no_op);
        assert_eq!(d.disposition, Some(Disposition::OutOfScope));
        assert!(d.apply_labels.contains(&"looper:out-of-scope".to_string()));
    }

    #[test]
    fn test_decide_unclear() {
        let llm = MockLlm(r#"{"disposition":"unclear","comment":"This issue needs more detail.","labels":{}}"#);
        let input = Input {
            issue: sample_issue(),
            repo_context: RepoContext::default(),
            config: Config::default(),
            now: Utc.with_ymd_and_hms(2026, 6, 21, 12, 0, 0).unwrap(),
        };
        let d = decide(Some(&llm), &input);
        assert!(!d.no_op);
        assert_eq!(d.disposition, Some(Disposition::Unclear));
        assert!(d.apply_labels.contains(&"looper:needs-human".to_string()));
    }

    #[test]
    fn test_decide_no_llm_returns_noop() {
        let input = Input {
            issue: sample_issue(),
            repo_context: RepoContext::default(),
            config: Config::default(),
            now: Utc.with_ymd_and_hms(2026, 6, 21, 12, 0, 0).unwrap(),
        };
        let d = decide(None, &input);
        assert!(d.no_op);
    }

    #[test]
    fn test_should_triage_not_triaged() {
        let issue = sample_issue();
        let cfg = Config::default();
        let now = Utc.with_ymd_and_hms(2026, 6, 21, 12, 0, 0).unwrap();
        assert!(should_triage(&issue, &cfg, now));
    }

    #[test]
    fn test_should_triage_already_triaged() {
        let mut issue = sample_issue();
        issue.labels.push("looper:triaged".into());
        let cfg = Config::default();
        let now = Utc.with_ymd_and_hms(2026, 6, 21, 12, 0, 0).unwrap();
        assert!(!should_triage(&issue, &cfg, now));
    }

    #[test]
    fn test_should_triage_too_old() {
        let issue = sample_issue();
        let cfg = Config::default();
        let now = Utc.with_ymd_and_hms(2026, 8, 1, 12, 0, 0).unwrap(); // >30 days
        assert!(!should_triage(&issue, &cfg, now));
    }

    #[test]
    fn test_should_re_triage_author_replied() {
        let mut issue = sample_issue();
        issue.labels.push("looper:needs-human".into());
        issue.timeline.push(TimelineEvent {
            event: "labeled".into(),
            created_at: "2026-06-20T13:00:00Z".into(),
            label: "looper:needs-human".into(),
        });
        issue.comments.push(Comment {
            id: 1,
            author: "user1".into(),
            author_association: "OWNER".into(),
            body: "I added more details!".into(),
            created_at: "2026-06-20T14:00:00Z".into(),
            updated_at: "2026-06-20T14:00:00Z".into(),
        });
        let cfg = Config::default();
        let now = Utc.with_ymd_and_hms(2026, 6, 21, 12, 0, 0).unwrap();
        assert!(should_re_triage(&issue, &cfg, now));
    }

    #[test]
    fn test_build_prompt_contains_issue() {
        let input = Input {
            issue: sample_issue(),
            repo_context: RepoContext {
                paths: vec!["src/auth.rs".into()],
                symbols: vec!["login".into()],
                ..Default::default()
            },
            config: Config::default(),
            now: Utc.with_ymd_and_hms(2026, 6, 21, 12, 0, 0).unwrap(),
        };
        let prompt = build_prompt(&input);
        assert!(prompt.contains("Add login page"));
        assert!(prompt.contains("src/auth.rs"));
        assert!(prompt.contains("login"));
    }

    #[test]
    fn test_parse_time_rfc3339() {
        assert!(parse_time("2026-06-20T12:00:00Z").is_some());
    }

    #[test]
    fn test_parse_time_empty() {
        assert!(parse_time("").is_none());
    }

    #[test]
    fn test_search_tokens() {
        let issue = sample_issue();
        let tokens = search_tokens(&issue);
        assert!(!tokens.is_empty());
        assert!(tokens.contains(&"login".to_string()));
    }

    #[test]
    fn test_limit_per_tick() {
        let items = vec![1, 2, 3, 4, 5];
        assert_eq!(limit_per_tick(&items, 3).len(), 3);
        assert_eq!(limit_per_tick(&items, 10).len(), 5);
    }

    #[test]
    fn test_allowed_label_universe() {
        let u = allowed_label_universe();
        assert!(u.contains(&"kind/feature"));
        assert!(u.contains(&"dispatch/plan"));
        assert!(u.contains(&"complexity/m"));
    }

    #[test]
    fn test_no_op_decision() {
        let d = no_op_decision();
        assert!(d.no_op);
    }

    #[test]
    fn test_re_triage_decision() {
        let cfg = Config::default();
        let d = re_triage_decision(&cfg);
        assert_eq!(d.disposition, Some(Disposition::Unclear));
        assert!(d.remove_labels.contains(&"looper:needs-human".to_string()));
    }
}
