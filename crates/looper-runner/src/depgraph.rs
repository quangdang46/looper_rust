//! Dependency graph engine for multi-issue coordination.
//!
//! Ported from Go `legacy/internal/coordinator/depgraph/depgraph.go`.
//!
//! Builds a DAG from GitHub issue dependency relationships. Determines
//! which issues are ready to work on (all deps satisfied), which are
//! blocked, and whether any dependency cycles exist.

use std::collections::{HashMap, HashSet};
use std::fmt;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A reference to a GitHub issue in a repository.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct IssueRef {
    pub repo: String,
    pub number: i64,
}

impl fmt::Display for IssueRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let r = self.normalize();
        if r.number <= 0 {
            return Ok(());
        }
        if r.repo.is_empty() {
            write!(f, "#{}", r.number)
        } else {
            write!(f, "{}#{}", r.repo, r.number)
        }
    }
}

impl IssueRef {
    fn normalize(&self) -> Self {
        Self { repo: self.repo.trim().to_lowercase(), number: self.number }
    }
}

/// State of an issue as known from GitHub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueState {
    pub state: String,
    pub state_reason: String,
}

/// A snapshot of dependency relationships and issue states.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// Map of issue → its direct dependencies (blocked-by relationships).
    pub blocked_by: HashMap<IssueRef, Vec<IssueRef>>,
    /// Known issue states.
    pub issues: HashMap<IssueRef, IssueState>,
    /// References that are known to be unreachable/non-existent.
    pub unreachable: Vec<IssueRef>,
}

/// A single blocker dependency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blocker {
    pub issue: IssueRef,
    pub state: String,
    pub state_reason: String,
    pub satisfied: bool,
    pub requires_re_triage: bool,
    pub unreachable: bool,
    /// Populated by [`DependencyGraph::unsatisfied`].
    pub number: i64,
    /// Populated by [`DependencyGraph::unsatisfied`].
    pub repo: String,
    /// Populated by [`DependencyGraph::unsatisfied`].
    pub reachable: bool,
}

/// A cycle in the dependency graph (ordered list of refs).
pub type Cycle = Vec<IssueRef>;

/// The computed dependency graph.
#[derive(Debug, Clone)]
pub struct DependencyGraph {
    ready_set: Vec<IssueRef>,
    cycles: Vec<Cycle>,
    unreachable: Vec<IssueRef>,
    blockers: HashMap<IssueRef, Vec<Blocker>>,
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// Build a dependency graph from the given tracked issues and snapshot.
pub fn build(tracked: &[IssueRef], snapshot: Snapshot) -> DependencyGraph {
    let tracked = unique_sorted_refs(tracked);
    let tracked_set: HashSet<IssueRef> = tracked.iter().cloned().collect();

    let blocked_by = normalize_blocked_by(&snapshot.blocked_by);
    let issue_states = normalize_issue_states(&snapshot.issues);
    let mut unreachable_set: HashSet<IssueRef> = snapshot
        .unreachable
        .iter()
        .filter_map(|r| {
            let r = r.normalize();
            if r.number > 0 {
                Some(r)
            } else {
                None
            }
        })
        .collect();

    let mut ready_set: Vec<IssueRef> = Vec::new();
    let mut blockers: HashMap<IssueRef, Vec<Blocker>> = HashMap::new();
    let mut edges: HashMap<IssueRef, Vec<IssueRef>> = HashMap::new();

    for issue in &tracked {
        let deps = unique_sorted_refs(blocked_by.get(issue).map(Vec::as_slice).unwrap_or_default());
        let mut unsatisfied: Vec<Blocker> = Vec::new();
        let mut tracked_deps: Vec<IssueRef> = Vec::new();

        for dep in &deps {
            let blocker = new_blocker(dep, &issue_states);
            if blocker.satisfied {
                continue;
            }
            let is_unreachable = blocker.unreachable;
            unsatisfied.push(blocker);
            if is_unreachable {
                unreachable_set.insert(dep.clone());
                continue;
            }
            if tracked_set.contains(dep) {
                tracked_deps.push(dep.clone());
            }
        }

        if unsatisfied.is_empty() {
            ready_set.push(issue.clone());
        } else {
            blockers.insert(issue.clone(), unsatisfied);
            if !tracked_deps.is_empty() {
                edges.insert(issue.clone(), tracked_deps);
            }
        }
    }

    DependencyGraph {
        ready_set: ready_set.clone(),
        cycles: detect_cycles(&tracked, &edges),
        unreachable: refs_from_set(&unreachable_set),
        blockers,
    }
}

// ---------------------------------------------------------------------------
// Graph query methods
// ---------------------------------------------------------------------------

impl DependencyGraph {
    /// Issues that have all dependencies satisfied and are ready to work on.
    pub fn ready_set(&self) -> Vec<IssueRef> {
        self.ready_set.clone()
    }

    /// Dependency cycles found in the tracked issues.
    pub fn cycles(&self) -> Vec<Cycle> {
        self.cycles.clone()
    }

    /// References that point to non-existent issues.
    pub fn unreachable_deps(&self) -> Vec<IssueRef> {
        self.unreachable.clone()
    }

    /// Blockers for a specific issue.
    pub fn blockers_of(&self, issue: &IssueRef) -> Vec<Blocker> {
        let r = issue.normalize();
        match self.blockers.get(&r) {
            Some(rows) => rows.clone(),
            None => vec![],
        }
    }

    /// Get unsatisfied blockers for an issue by number (ignoring repo).
    pub fn unsatisfied(&self, issue_number: i64) -> Vec<Blocker> {
        if issue_number <= 0 {
            return vec![];
        }
        for (issue, blockers) in &self.blockers {
            if issue.number != issue_number || blockers.is_empty() {
                continue;
            }
            let mut out: Vec<Blocker> = blockers.clone();
            for ref mut blocker in &mut out {
                blocker.number = blocker.issue.number;
                blocker.repo = blocker.issue.repo.clone();
                blocker.reachable = !blocker.unreachable;
            }
            return out;
        }
        vec![]
    }
}

// ---------------------------------------------------------------------------
// Blocker classification
// ---------------------------------------------------------------------------

struct BlockerDisposition {
    satisfied: bool,
    requires_re_triage: bool,
}

fn classify_blocker_state(state: &IssueState) -> BlockerDisposition {
    let state_norm = state.state.trim().to_lowercase();
    let reason_norm = state.state_reason.trim().to_lowercase();
    if state_norm == "closed" && reason_norm == "completed" {
        return BlockerDisposition { satisfied: true, requires_re_triage: false };
    }
    if state_norm == "closed" && (reason_norm == "not_planned" || reason_norm == "duplicate") {
        return BlockerDisposition { satisfied: false, requires_re_triage: true };
    }
    BlockerDisposition { satisfied: false, requires_re_triage: false }
}

fn new_blocker(dep: &IssueRef, issues: &HashMap<IssueRef, IssueState>) -> Blocker {
    let dep = dep.normalize();
    match issues.get(&dep) {
        None => Blocker {
            issue: dep,
            state: String::new(),
            state_reason: String::new(),
            satisfied: false,
            requires_re_triage: false,
            unreachable: true,
            number: 0,
            repo: String::new(),
            reachable: false,
        },
        Some(state) => {
            let state_norm = IssueState {
                state: state.state.trim().to_lowercase(),
                state_reason: state.state_reason.trim().to_lowercase(),
            };
            let classification = classify_blocker_state(&state_norm);
            Blocker {
                issue: dep,
                state: state_norm.state,
                state_reason: state_norm.state_reason,
                satisfied: classification.satisfied,
                requires_re_triage: classification.requires_re_triage,
                unreachable: false,
                number: 0,
                repo: String::new(),
                reachable: true,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Cycle detection
// ---------------------------------------------------------------------------

fn detect_cycles(tracked: &[IssueRef], edges: &HashMap<IssueRef, Vec<IssueRef>>) -> Vec<Cycle> {
    let mut state: HashMap<IssueRef, u8> = HashMap::new();
    let mut stack: Vec<IssueRef> = Vec::new();
    let mut stack_index: HashMap<IssueRef, usize> = HashMap::new();
    let mut seen: HashMap<String, Cycle> = HashMap::new();

    fn visit(
        node: &IssueRef,
        tracked: &[IssueRef],
        edges: &HashMap<IssueRef, Vec<IssueRef>>,
        state: &mut HashMap<IssueRef, u8>,
        stack: &mut Vec<IssueRef>,
        stack_index: &mut HashMap<IssueRef, usize>,
        seen: &mut HashMap<String, Cycle>,
    ) {
        state.insert(node.clone(), 1);
        stack_index.insert(node.clone(), stack.len());
        stack.push(node.clone());

        if let Some(nexts) = edges.get(node) {
            for next in nexts {
                if let Some(&idx) = stack_index.get(next) {
                    let mut cycle: Cycle = stack[idx..].to_vec();
                    cycle.push(next.clone());
                    let normalized = canonicalize_cycle(&cycle);
                    seen.entry(cycle_key(&normalized)).or_insert_with(|| normalized);
                    continue;
                }
                if *state.get(next).unwrap_or(&0) == 0 {
                    visit(next, tracked, edges, state, stack, stack_index, seen);
                }
            }
        }

        stack.pop();
        stack_index.remove(node);
        state.insert(node.clone(), 2);
    }

    for node in tracked {
        if *state.get(node).unwrap_or(&0) == 0 {
            visit(node, tracked, edges, &mut state, &mut stack, &mut stack_index, &mut seen);
        }
    }

    let mut keys: Vec<&String> = seen.keys().collect();
    keys.sort();
    keys.iter().map(|k| seen[k.as_str()].clone()).collect()
}

fn canonicalize_cycle(cycle: &[IssueRef]) -> Cycle {
    if cycle.len() < 2 {
        return vec![];
    }
    let nodes: Vec<IssueRef> = cycle[..cycle.len() - 1].to_vec();
    let mut best = nodes.clone();
    for index in 1..nodes.len() {
        let candidate: Vec<IssueRef> = nodes[index..].iter().chain(nodes[..index].iter()).cloned().collect();
        if refs_slice_less(&candidate, &best) {
            best = candidate;
        }
    }
    let mut result = best;
    result.push(result[0].clone());
    result
}

fn refs_slice_less(left: &[IssueRef], right: &[IssueRef]) -> bool {
    for i in 0..left.len().min(right.len()) {
        if ref_less(&left[i], &right[i]) {
            return true;
        }
        if ref_less(&right[i], &left[i]) {
            return false;
        }
    }
    left.len() < right.len()
}

fn cycle_key(cycle: &[IssueRef]) -> String {
    cycle.iter().map(|r| r.to_string()).collect::<Vec<_>>().join("->")
}

// ---------------------------------------------------------------------------
// Normalization helpers
// ---------------------------------------------------------------------------

fn normalize_blocked_by(input: &HashMap<IssueRef, Vec<IssueRef>>) -> HashMap<IssueRef, Vec<IssueRef>> {
    let mut out: HashMap<IssueRef, Vec<IssueRef>> = HashMap::new();
    for (issue, deps) in input {
        let issue = issue.normalize();
        if issue.number <= 0 {
            continue;
        }
        for dep in deps {
            let dep = dep.normalize();
            if dep.number <= 0 {
                continue;
            }
            out.entry(issue.clone()).or_insert_with(Vec::new).push(dep);
        }
    }
    out
}

fn normalize_issue_states(input: &HashMap<IssueRef, IssueState>) -> HashMap<IssueRef, IssueState> {
    let mut out: HashMap<IssueRef, IssueState> = HashMap::new();
    for (ref_, state) in input {
        let ref_ = ref_.normalize();
        if ref_.number <= 0 {
            continue;
        }
        out.insert(
            ref_,
            IssueState { state: state.state.trim().to_string(), state_reason: state.state_reason.trim().to_string() },
        );
    }
    out
}

fn unique_sorted_refs(input: &[IssueRef]) -> Vec<IssueRef> {
    let mut set: HashSet<IssueRef> = HashSet::new();
    for ref_ in input {
        let r = ref_.normalize();
        if r.number > 0 {
            set.insert(r);
        }
    }
    refs_from_set(&set)
}

fn refs_from_set(set: &HashSet<IssueRef>) -> Vec<IssueRef> {
    let mut out: Vec<IssueRef> = set.iter().cloned().collect();
    out.sort_by(|a, b| if a.repo != b.repo { a.repo.cmp(&b.repo) } else { a.number.cmp(&b.number) });
    out
}

fn ref_less(left: &IssueRef, right: &IssueRef) -> bool {
    let left = left.normalize();
    let right = right.normalize();
    if left.repo != right.repo {
        return left.repo < right.repo;
    }
    left.number < right.number
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> IssueRef {
        IssueRef { repo: "acme/looper".into(), number: 0 }
    }
    fn issue(n: i64) -> IssueRef {
        IssueRef { repo: "acme/looper".into(), number: n }
    }

    #[test]
    fn test_ready_set_empty() {
        let g = build(&[], Snapshot::default());
        assert_eq!(g.ready_set().len(), 0);
    }

    #[test]
    fn test_ready_set_single_no_blockers() {
        let g = build(&[issue(1)], Snapshot::default());
        assert_eq!(g.ready_set(), vec![issue(1)]);
    }

    #[test]
    fn test_ready_set_open_blocker() {
        let b = issue(2);
        let g = build(
            &[issue(1)],
            Snapshot {
                blocked_by: HashMap::from([(issue(1), vec![b.clone()])]),
                issues: HashMap::from([(b, IssueState { state: "open".into(), state_reason: String::new() })]),
                ..Default::default()
            },
        );
        assert!(g.ready_set().is_empty());
    }

    #[test]
    fn test_ready_set_closed_completed_blocker() {
        let b = issue(2);
        let g = build(
            &[issue(1)],
            Snapshot {
                blocked_by: HashMap::from([(issue(1), vec![b.clone()])]),
                issues: HashMap::from([(b, IssueState { state: "closed".into(), state_reason: "completed".into() })]),
                ..Default::default()
            },
        );
        assert_eq!(g.ready_set(), vec![issue(1)]);
    }

    #[test]
    fn test_two_node_cycle() {
        let g = build(
            &[issue(1), issue(2)],
            Snapshot {
                blocked_by: HashMap::from([(issue(1), vec![issue(2)]), (issue(2), vec![issue(1)])]),
                issues: HashMap::from([
                    (issue(1), IssueState { state: "open".into(), state_reason: String::new() }),
                    (issue(2), IssueState { state: "open".into(), state_reason: String::new() }),
                ]),
                ..Default::default()
            },
        );
        let cycles = g.cycles();
        assert_eq!(cycles.len(), 1);
        assert_eq!(cycles[0].len(), 3); // [1, 2, 1]
    }

    #[test]
    fn test_three_node_cycle() {
        let g = build(
            &[issue(1), issue(2), issue(3)],
            Snapshot {
                blocked_by: HashMap::from([
                    (issue(1), vec![issue(2)]),
                    (issue(2), vec![issue(3)]),
                    (issue(3), vec![issue(1)]),
                ]),
                issues: HashMap::from([
                    (issue(1), IssueState { state: "open".into(), state_reason: String::new() }),
                    (issue(2), IssueState { state: "open".into(), state_reason: String::new() }),
                    (issue(3), IssueState { state: "open".into(), state_reason: String::new() }),
                ]),
                ..Default::default()
            },
        );
        assert_eq!(g.cycles().len(), 1);
    }

    #[test]
    fn test_self_loop() {
        let g = build(
            &[issue(1)],
            Snapshot {
                blocked_by: HashMap::from([(issue(1), vec![issue(1)])]),
                issues: HashMap::from([(issue(1), IssueState { state: "open".into(), state_reason: String::new() })]),
                ..Default::default()
            },
        );
        assert_eq!(g.cycles().len(), 1);
    }

    #[test]
    fn test_unreachable_deps() {
        let g = build(
            &[issue(1)],
            Snapshot {
                blocked_by: HashMap::from([(
                    issue(1),
                    vec![issue(2), IssueRef { repo: "other/repo".into(), number: 9 }],
                )]),
                issues: HashMap::new(),
                ..Default::default()
            },
        );
        let unreachable = g.unreachable_deps();
        assert_eq!(unreachable.len(), 2);
    }

    #[test]
    fn test_blocker_of_returns_only_unsatisfied() {
        // issue(1) depends on 2 (completed), 3 (open), 4 (not_planned)
        let g = build(
            &[issue(1)],
            Snapshot {
                blocked_by: HashMap::from([(issue(1), vec![issue(2), issue(3), issue(4)])]),
                issues: HashMap::from([
                    (issue(2), IssueState { state: "closed".into(), state_reason: "completed".into() }),
                    (issue(3), IssueState { state: "open".into(), state_reason: String::new() }),
                    (issue(4), IssueState { state: "closed".into(), state_reason: "not_planned".into() }),
                ]),
                ..Default::default()
            },
        );
        let blockers = g.blockers_of(&issue(1));
        assert_eq!(blockers.len(), 2);
        // issue(3) is open → blocker
        assert!(blockers.iter().any(|b| b.issue.number == 3 && !b.satisfied));
        // issue(4) is not_planned → requires re-triage
        assert!(blockers.iter().any(|b| b.issue.number == 4 && b.requires_re_triage));
    }

    #[test]
    fn test_classify_completed() {
        let r = classify_blocker_state(&IssueState { state: "closed".into(), state_reason: "completed".into() });
        assert!(r.satisfied);
        assert!(!r.requires_re_triage);
    }

    #[test]
    fn test_classify_not_planned() {
        let r = classify_blocker_state(&IssueState { state: "closed".into(), state_reason: "not_planned".into() });
        assert!(!r.satisfied);
        assert!(r.requires_re_triage);
    }

    #[test]
    fn test_classify_open() {
        let r = classify_blocker_state(&IssueState { state: "open".into(), state_reason: String::new() });
        assert!(!r.satisfied);
        assert!(!r.requires_re_triage);
    }

    #[test]
    fn test_unsatisfied_by_number() {
        let g = build(
            &[issue(1)],
            Snapshot {
                blocked_by: HashMap::from([(issue(1), vec![issue(2)])]),
                issues: HashMap::from([(issue(2), IssueState { state: "open".into(), state_reason: String::new() })]),
                ..Default::default()
            },
        );
        let u = g.unsatisfied(1);
        assert_eq!(u.len(), 1);
        assert_eq!(u[0].number, 2);
    }

    #[test]
    fn test_issue_ref_display() {
        let r = IssueRef { repo: "acme/looper".into(), number: 42 };
        assert_eq!(r.to_string(), "acme/looper#42");
    }

    #[test]
    fn test_issue_ref_display_no_repo() {
        let r = IssueRef { repo: "".into(), number: 42 };
        assert_eq!(r.to_string(), "#42");
    }

    #[test]
    fn test_normalize_case_sensitivity() {
        let r1 = IssueRef { repo: "Acme/Looper".into(), number: 1 };
        let r2 = IssueRef { repo: "acme/looper".into(), number: 1 };
        assert_eq!(r1.normalize(), r2);
    }
}
