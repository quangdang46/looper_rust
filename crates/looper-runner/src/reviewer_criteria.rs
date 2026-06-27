//! Review criteria: extract acceptance criteria from issue body and verify
//! against a PR diff using token matching.
//!
//! Ported from Go `legacy/internal/reviewer/criteria/` (357 LOC).

use std::collections::HashSet;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A single acceptance criterion extracted from an issue body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptanceCriterion(pub String);

/// Verdict for a single criterion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Fail,
    Unverifiable,
}

/// Overall verification result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggregateDisposition {
    Pass,
    Fail,
    Unverifiable,
}

/// Evidence supporting a criterion verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evidence {
    pub file_path: String,
    pub start_line: usize,
    pub end_line: usize,
}

/// A single file in a PR diff.
#[derive(Debug, Clone)]
pub struct DiffFile {
    pub path: String,
    pub patch: String,
}

/// A PR diff containing multiple files.
#[derive(Debug, Clone)]
pub struct PRDiff {
    pub files: Vec<DiffFile>,
}

/// Assessment of a single criterion against the diff.
#[derive(Debug, Clone)]
pub struct CriterionAssessment {
    pub verdict: Verdict,
    pub justification: String,
    pub evidence: Vec<Evidence>,
}

/// Result of verifying a single criterion.
#[derive(Debug, Clone)]
pub struct CriterionResult {
    pub criterion: AcceptanceCriterion,
    pub verdict: Verdict,
    pub justification: String,
    pub evidence: Vec<Evidence>,
}

/// Overall verification result for all criteria.
#[derive(Debug, Clone)]
pub struct VerificationResult {
    pub disposition: AggregateDisposition,
    pub criteria: Vec<CriterionResult>,
}

/// The verifier trait.
pub trait Verifier {
    fn verify_criterion(&self, criterion: &AcceptanceCriterion, diff: &PRDiff) -> Result<CriterionAssessment, String>;
}

// ---------------------------------------------------------------------------
// Extraction
// ---------------------------------------------------------------------------

/// Extract acceptance criteria from an issue body (markdown).
///
/// Looks for "## Acceptance Criteria" heading followed by markdown list items.
pub fn extract(issue_body: &str) -> Vec<AcceptanceCriterion> {
    let lines: Vec<&str> = issue_body.lines().collect();
    let mut in_section = false;
    let mut section_level = 0;
    let mut criteria = Vec::new();

    for &raw_line in &lines {
        let trimmed = raw_line.trim();
        if let Some((level, heading)) = markdown_heading(trimmed) {
            if is_acceptance_criteria_heading(heading) {
                in_section = true;
                section_level = level;
                continue;
            }
            if in_section && level <= section_level {
                break;
            }
            continue;
        }
        if !in_section || trimmed.is_empty() {
            continue;
        }
        if let Some(criterion) = parse_criterion_line(trimmed) {
            criteria.push(AcceptanceCriterion(criterion));
        }
    }

    criteria
}

/// Verify all criteria against the diff using the given verifier.
pub fn verify(
    criteria: &[AcceptanceCriterion],
    diff: &PRDiff,
    verifier: &dyn Verifier,
) -> Result<VerificationResult, String> {
    let mut results = Vec::with_capacity(criteria.len());
    let mut disposition = AggregateDisposition::Pass;

    for criterion in criteria {
        let assessment = verifier.verify_criterion(criterion, diff)?;
        validate_assessment(criterion, &assessment, diff)?;
        results.push(CriterionResult {
            criterion: criterion.clone(),
            verdict: assessment.verdict,
            justification: assessment.justification,
            evidence: assessment.evidence,
        });
        match assessment.verdict {
            Verdict::Fail => disposition = AggregateDisposition::Fail,
            Verdict::Unverifiable => {
                if !matches!(disposition, AggregateDisposition::Fail) {
                    disposition = AggregateDisposition::Unverifiable;
                }
            }
            Verdict::Pass => {}
        }
    }

    Ok(VerificationResult { disposition, criteria: results })
}

// ---------------------------------------------------------------------------
// Default verifier
// ---------------------------------------------------------------------------

/// A simple token-overlap-based verifier.
#[derive(Debug, Clone)]
pub struct DefaultVerifier;

impl DefaultVerifier {
    pub fn new() -> Self {
        Self
    }
}

impl Verifier for DefaultVerifier {
    fn verify_criterion(&self, criterion: &AcceptanceCriterion, diff: &PRDiff) -> Result<CriterionAssessment, String> {
        let added = collect_added_lines(diff);
        let criterion_text = criterion.0.trim().to_lowercase();
        let criterion_toks = criterion_tokens(&criterion_text);

        for al in &added {
            let line_lower = al.text.trim().to_lowercase();
            if line_lower.is_empty() {
                continue;
            }
            if line_lower.contains(&criterion_text) || token_overlap(&criterion_toks, &criterion_tokens(&line_lower)) {
                return Ok(CriterionAssessment {
                    verdict: Verdict::Pass,
                    justification: "diff contains matching implementation evidence".into(),
                    evidence: vec![Evidence {
                        file_path: al.file_path.clone(),
                        start_line: al.line,
                        end_line: al.line,
                    }],
                });
            }
        }

        Ok(CriterionAssessment {
            verdict: Verdict::Unverifiable,
            justification: "diff does not contain deterministic evidence matching this criterion".into(),
            evidence: vec![],
        })
    }
}

// ---------------------------------------------------------------------------
// Auto-merge decision
// ---------------------------------------------------------------------------

/// Reason for refusing auto-merge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalReason {
    Disabled,
    Scope,
    NoBranchProtection,
    StrategyDisallowed,
    AutoMergeDisabled,
}

/// Snapshot of the PR for auto-merge decision.
#[derive(Debug, Clone)]
pub struct PRSnapshot {
    pub labels: Vec<String>,
    pub has_tracked_issue_link: bool,
}

/// Snapshot of branch protection settings.
#[derive(Debug, Clone)]
pub struct BranchProtectionSnapshot {
    pub exists: bool,
    pub has_required_checks: bool,
}

/// Snapshot of repo settings.
#[derive(Debug, Clone)]
pub struct RepoSettingsSnapshot {
    pub allow_squash_merge: bool,
    pub allow_merge_commit: bool,
    pub allow_rebase_merge: bool,
    pub allow_auto_merge: bool,
}

/// Auto-merge strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoMergeStrategy {
    Squash,
    Merge,
    Rebase,
}

/// Decision about auto-merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoMergeDecision {
    OptIn(AutoMergeStrategy),
    Refuse(RefusalReason),
}

/// Decide whether a PR qualifies for auto-merge.
pub fn decide_auto_merge(
    pr: &PRSnapshot,
    auto_merge_enabled: bool,
    strategy: AutoMergeStrategy,
    require_branch_protection: bool,
    protection: &BranchProtectionSnapshot,
    settings: &RepoSettingsSnapshot,
) -> AutoMergeDecision {
    if !auto_merge_enabled {
        return AutoMergeDecision::Refuse(RefusalReason::Disabled);
    }
    if !has_looper_label(&pr.labels) || !pr.has_tracked_issue_link {
        return AutoMergeDecision::Refuse(RefusalReason::Scope);
    }
    if require_branch_protection && (!protection.exists || !protection.has_required_checks) {
        return AutoMergeDecision::Refuse(RefusalReason::NoBranchProtection);
    }
    if !strategy_allowed(strategy, settings) {
        return AutoMergeDecision::Refuse(RefusalReason::StrategyDisallowed);
    }
    if !settings.allow_auto_merge {
        return AutoMergeDecision::Refuse(RefusalReason::AutoMergeDisabled);
    }
    AutoMergeDecision::OptIn(strategy)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

struct AddedLine {
    file_path: String,
    line: usize,
    text: String,
}

fn collect_added_lines(diff: &PRDiff) -> Vec<AddedLine> {
    let mut lines = Vec::new();
    for file in &diff.files {
        let mut line_no: usize = 0;
        for raw in file.patch.lines() {
            if raw.starts_with("@@") {
                line_no = hunk_start(raw);
                continue;
            }
            if raw.starts_with('+') && !raw.starts_with("+++") {
                let text = if raw.len() > 1 { &raw[1..] } else { "" };
                lines.push(AddedLine {
                    file_path: file.path.clone(),
                    line: if line_no < 1 { 1 } else { line_no },
                    text: text.to_string(),
                });
                line_no = line_no.saturating_add(1);
                continue;
            }
            if raw.starts_with('-') && !raw.starts_with("---") {
                continue;
            }
            if !raw.is_empty() {
                line_no = line_no.saturating_add(1);
            }
        }
    }
    lines
}

fn hunk_start(raw: &str) -> usize {
    for part in raw.split(' ') {
        if !part.starts_with('+') {
            continue;
        }
        let p = part.trim_start_matches('+');
        let p = p.trim_end_matches('@');
        let p = p.split(',').next().unwrap_or(p).trim();
        if p.is_empty() {
            return 0;
        }
        return p.parse::<usize>().unwrap_or(0);
    }
    0
}

fn criterion_tokens(value: &str) -> Vec<String> {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .split(|c: char| !c.is_ascii_alphanumeric() && c != ' ')
        .flat_map(|s| if s.len() >= 4 { Some(s.to_lowercase()) } else { None })
        .collect()
}

fn token_overlap(a: &[String], b: &[String]) -> bool {
    if a.len() < 2 || b.is_empty() {
        return false;
    }
    let mut set: HashSet<&str> = a.iter().map(|s| s.as_str()).collect();
    let mut matches = 0;
    for token in b {
        if set.remove(token.as_str()) {
            matches += 1;
        }
    }
    matches >= 2 || matches >= a.len().min(2)
}

fn parse_criterion_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    if !trimmed.starts_with('-') && !trimmed.starts_with('*') {
        return None;
    }
    let after_bullet = trimmed[1..].trim();
    let result = trim_checkbox_prefix(after_bullet);
    if result.is_empty() {
        return None;
    }
    Some(result)
}

fn is_acceptance_criteria_heading(heading: &str) -> bool {
    let h = heading.trim().trim_end_matches(|c: char| c == ':' || c == ';' || c == '.' || c == '!' || c == '?').trim();
    h.eq_ignore_ascii_case("acceptance criteria")
}

fn markdown_heading(line: &str) -> Option<(usize, &str)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || !trimmed.starts_with('#') {
        return None;
    }
    let mut level = 0;
    for c in trimmed.chars() {
        if c == '#' {
            level += 1;
        } else {
            break;
        }
    }
    if level == 0 || level > 6 || level >= trimmed.len() || !trimmed[level..].starts_with(' ') {
        return None;
    }
    Some((level, trimmed[level + 1..].trim()))
}

fn trim_checkbox_prefix(line: &str) -> String {
    let trimmed = line.trim();
    for prefix in &["[ ]", "[x]", "[X]", "[]"] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            return rest.trim().to_string();
        }
    }
    trimmed.to_string()
}

fn has_looper_label(labels: &[String]) -> bool {
    labels.iter().any(|l| l.to_lowercase().trim().starts_with("looper:"))
}

fn strategy_allowed(strategy: AutoMergeStrategy, settings: &RepoSettingsSnapshot) -> bool {
    match strategy {
        AutoMergeStrategy::Squash => settings.allow_squash_merge,
        AutoMergeStrategy::Merge => settings.allow_merge_commit,
        AutoMergeStrategy::Rebase => settings.allow_rebase_merge,
    }
}

fn validate_assessment(
    criterion: &AcceptanceCriterion,
    assessment: &CriterionAssessment,
    diff: &PRDiff,
) -> Result<(), String> {
    match assessment.verdict {
        Verdict::Pass | Verdict::Fail | Verdict::Unverifiable => {}
    }
    if assessment.justification.trim().is_empty() {
        return Err(format!("criterion {:?} returned empty justification", criterion.0));
    }
    if assessment.verdict != Verdict::Pass {
        return Ok(());
    }
    if assessment.evidence.is_empty() {
        return Err(format!("criterion {:?} returned pass without evidence", criterion.0));
    }
    for evidence in &assessment.evidence {
        if evidence.file_path.trim().is_empty() || evidence.start_line < 1 || evidence.end_line < evidence.start_line {
            return Err(format!("criterion {:?} returned invalid evidence", criterion.0));
        }
        if !diff_contains_evidence(diff, evidence) {
            return Err(format!("criterion {:?} returned pass evidence outside the diff", criterion.0));
        }
    }
    Ok(())
}

fn diff_contains_evidence(diff: &PRDiff, evidence: &Evidence) -> bool {
    for file in &diff.files {
        if file.path != evidence.file_path {
            continue;
        }
        // Simple check: evidence file path exists in diff
        return true;
    }
    false
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_simple() {
        let body = "## Acceptance Criteria\n- [ ] User can log in\n- [ ] Error message on failure\n";
        let criteria = extract(body);
        assert_eq!(criteria.len(), 2);
        assert_eq!(criteria[0].0, "User can log in");
    }

    #[test]
    fn test_extract_no_criteria() {
        let body = "Just a regular issue body.";
        let criteria = extract(body);
        assert!(criteria.is_empty());
    }

    #[test]
    fn test_extract_nested_heading() {
        let body = "## Feature\n### Acceptance Criteria\n- [ ] Works\n";
        let criteria = extract(body);
        assert_eq!(criteria.len(), 1);
        assert_eq!(criteria[0].0, "Works");
    }

    #[test]
    fn test_default_verifier_pass() {
        let diff = PRDiff {
            files: vec![DiffFile {
                path: "src/auth.rs".into(),
                patch: "@@ -0,0 +1 @@\n+fn login() { /* User can log in */ }\n".into(),
            }],
        };
        let verifier = DefaultVerifier::new();
        let criterion = AcceptanceCriterion("User can log in".into());
        let result = verifier.verify_criterion(&criterion, &diff).unwrap();
        assert_eq!(result.verdict, Verdict::Pass);
    }

    #[test]
    fn test_default_verifier_unverifiable() {
        let diff = PRDiff {
            files: vec![DiffFile { path: "src/other.rs".into(), patch: "@@ -0,0 +1 @@\n+fn unrelated() {}\n".into() }],
        };
        let verifier = DefaultVerifier::new();
        let criterion = AcceptanceCriterion("User can log in".into());
        let result = verifier.verify_criterion(&criterion, &diff).unwrap();
        assert_eq!(result.verdict, Verdict::Unverifiable);
    }

    #[test]
    fn test_verify_empty_criteria_returns_pass() {
        let diff = PRDiff { files: vec![] };
        let verifier = DefaultVerifier::new();
        let result = verify(&[], &diff, &verifier).unwrap();
        assert_eq!(result.disposition, AggregateDisposition::Pass);
        assert!(result.criteria.is_empty());
    }

    #[test]
    fn test_verify_mixed() {
        let diff = PRDiff {
            files: vec![DiffFile {
                path: "src/auth.rs".into(),
                patch: "@@ -0,0 +1 @@\n+fn login() { /* User can log in */ }\n".into(),
            }],
        };
        let verifier = DefaultVerifier::new();
        let criteria =
            vec![AcceptanceCriterion("User can log in".into()), AcceptanceCriterion("Send email notification".into())];
        let result = verify(&criteria, &diff, &verifier).unwrap();
        assert_eq!(result.disposition, AggregateDisposition::Unverifiable);
        assert_eq!(result.criteria.len(), 2);
        assert_eq!(result.criteria[0].verdict, Verdict::Pass);
        assert_eq!(result.criteria[1].verdict, Verdict::Unverifiable);
    }

    #[test]
    fn test_auto_merge_opt_in() {
        let pr = PRSnapshot { labels: vec!["looper:worker-ready".into()], has_tracked_issue_link: true };
        let protection = BranchProtectionSnapshot { exists: true, has_required_checks: true };
        let settings = RepoSettingsSnapshot {
            allow_squash_merge: true,
            allow_merge_commit: true,
            allow_rebase_merge: true,
            allow_auto_merge: true,
        };
        let d = decide_auto_merge(&pr, true, AutoMergeStrategy::Squash, true, &protection, &settings);
        assert_eq!(d, AutoMergeDecision::OptIn(AutoMergeStrategy::Squash));
    }

    #[test]
    fn test_auto_merge_disabled() {
        let pr = PRSnapshot { labels: vec!["looper:worker-ready".into()], has_tracked_issue_link: true };
        let protection = BranchProtectionSnapshot { exists: true, has_required_checks: true };
        let settings = RepoSettingsSnapshot {
            allow_squash_merge: true,
            allow_merge_commit: true,
            allow_rebase_merge: true,
            allow_auto_merge: true,
        };
        let d = decide_auto_merge(&pr, false, AutoMergeStrategy::Squash, true, &protection, &settings);
        assert_eq!(d, AutoMergeDecision::Refuse(RefusalReason::Disabled));
    }

    #[test]
    fn test_auto_merge_no_looper_label() {
        let pr = PRSnapshot { labels: vec!["bug".into()], has_tracked_issue_link: true };
        let protection = BranchProtectionSnapshot { exists: true, has_required_checks: true };
        let settings = RepoSettingsSnapshot {
            allow_squash_merge: true,
            allow_merge_commit: true,
            allow_rebase_merge: true,
            allow_auto_merge: true,
        };
        let d = decide_auto_merge(&pr, true, AutoMergeStrategy::Squash, true, &protection, &settings);
        assert_eq!(d, AutoMergeDecision::Refuse(RefusalReason::Scope));
    }

    #[test]
    fn test_auto_merge_no_branch_protection() {
        let pr = PRSnapshot { labels: vec!["looper:worker-ready".into()], has_tracked_issue_link: true };
        let protection = BranchProtectionSnapshot { exists: false, has_required_checks: false };
        let settings = RepoSettingsSnapshot {
            allow_squash_merge: true,
            allow_merge_commit: true,
            allow_rebase_merge: true,
            allow_auto_merge: true,
        };
        let d = decide_auto_merge(&pr, true, AutoMergeStrategy::Squash, true, &protection, &settings);
        assert_eq!(d, AutoMergeDecision::Refuse(RefusalReason::NoBranchProtection));
    }

    #[test]
    fn test_extract_checkbox_variants() {
        let body = "## Acceptance Criteria\n- [x] Done\n- [ ] Not done\n- [] Old format\n";
        let criteria = extract(body);
        assert_eq!(criteria.len(), 3);
    }

    #[test]
    fn test_has_looper_label() {
        assert!(has_looper_label(&["looper:worker-ready".into()]));
        assert!(!has_looper_label(&["bug".into()]));
    }

    #[test]
    fn test_markdown_heading() {
        assert_eq!(markdown_heading("## Hi").unwrap().0, 2);
        assert_eq!(markdown_heading("### Hi").unwrap().1, "Hi");
        assert!(markdown_heading("No heading").is_none());
    }
}
