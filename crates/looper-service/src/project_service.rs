use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde_json::json;
use tracing::{info, warn};

use looper_config::types::Config;
use looper_storage::{
    eventlog::append,
    record::{AppendInput, ProjectRecord, PullRequestSnapshotRecord, WorktreeRecord},
    repos::Repositories,
};

use crate::error::{Result, ServiceError};

/// ── Callback types ──────────────────────────────────────────────────────
/// Result type used for injection callbacks.
pub type CallbackResult<T> = std::result::Result<T, String>;

#[derive(Debug, Clone)]
pub struct WorktreeEntry {
    pub branch: String,
    pub worktree_path: String,
    pub head_sha: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PullRequestEntry {
    pub number: i64,
    pub title: Option<String>,
    pub body: Option<String>,
    pub author: Option<String>,
    pub head_sha: String,
    pub base_sha: Option<String>,
    pub draft: bool,
    pub state: String,
}

#[derive(Debug, Clone)]
pub struct RepositorySettings {
    pub allow_squash_merge: bool,
    pub allow_merge_commit: bool,
    pub allow_rebase_merge: bool,
    pub allow_auto_merge: bool,
}

#[derive(Debug, Clone)]
pub struct BranchProtection {
    pub enabled: bool,
    pub has_required_checks: bool,
}

/// Injection callbacks for IO operations that ProjectService delegates.
///
/// All callbacks are optional — when `None`, the corresponding operation is
/// skipped with a warning.  The actual implementations will be wired in
/// during Phase 5 (looper-github / looper-git).
pub struct ProjectServiceCallbacks {
    pub detect_repo: Option<Arc<dyn Fn(&str) -> CallbackResult<Option<String>> + Send + Sync>>,
    pub list_worktrees: Option<Arc<dyn Fn(&str) -> CallbackResult<Vec<WorktreeEntry>> + Send + Sync>>,
    pub list_open_pull_requests: Option<Arc<dyn Fn(&str, &str) -> CallbackResult<Vec<PullRequestEntry>> + Send + Sync>>,
    pub capture_pull_request_snapshot:
        Option<Arc<dyn Fn(&str, &str, i64, &str) -> CallbackResult<PullRequestSnapshotRecord> + Send + Sync>>,
    pub get_repository_settings: Option<Arc<dyn Fn(&str) -> CallbackResult<RepositorySettings> + Send + Sync>>,
    pub get_branch_protection: Option<Arc<dyn Fn(&str, &str) -> CallbackResult<BranchProtection> + Send + Sync>>,
}

impl ProjectServiceCallbacks {
    pub fn new() -> Self {
        Self {
            detect_repo: None,
            list_worktrees: None,
            list_open_pull_requests: None,
            capture_pull_request_snapshot: None,
            get_repository_settings: None,
            get_branch_protection: None,
        }
    }
}

impl Default for ProjectServiceCallbacks {
    fn default() -> Self {
        Self::new()
    }
}

/// ── ProjectService ──────────────────────────────────────────────────────
pub struct ProjectService {
    repos: Arc<Repositories>,
    callbacks: ProjectServiceCallbacks,
    now: Box<dyn Fn() -> DateTime<Utc>>,
}

impl ProjectService {
    pub fn new<F>(repos: Arc<Repositories>, callbacks: ProjectServiceCallbacks, now: F) -> Self
    where
        F: Fn() -> DateTime<Utc> + 'static,
    {
        Self { repos, callbacks, now: Box::new(now) }
    }

    // ── AddProject ──────────────────────────────────────────────────────

    pub fn add_project(&self, input: AddInput) -> Result<AddResult> {
        let now = (self.now)();
        let now_iso = now.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();

        let mut warnings: Vec<String> = Vec::new();

        // ── Phase 1: Validation & ID Normalization ─────────────────────

        // Check existing
        let existing = self.repos.projects.get_by_id(&input.id)?;

        if let Some(ref proj) = existing {
            if !proj.archived && input.id_source != "derived" {
                return Err(ServiceError::ProjectIDCollision(format!("project ID '{}' already exists", input.id,)));
            }
        }

        // Validate project ID
        validate_project_id(&input.id)?;

        // Normalize derived ID
        let normalized_id = if input.id_source == "derived" {
            let normalized: String = input
                .id
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
                .collect();
            // Collapse consecutive dashes
            let collapsed: String = normalized.chars().fold(String::new(), |mut acc, c| {
                if c == '-' && acc.ends_with('-') {
                    // skip
                } else {
                    acc.push(c);
                }
                acc
            });
            let collapsed = collapsed.trim_matches('-').to_string();
            if collapsed.is_empty() {
                return Err(ServiceError::InvalidProjectID("derived project ID is empty after normalization".into()));
            }
            collapsed
        } else {
            input.id.clone()
        };

        // Auto-detect repo (Phase 1c).
        // Canonical: explicit `input.repo` (owner/name or github URL) wins;
        // otherwise call `detect_repo` on the local checkout when wired.
        let repo = resolve_repo_for_add(&input, &self.callbacks, &mut warnings);

        // ── Phase 2: Reviewer Auto-Merge Validation ────────────────────
        // (Stubbed — will be wired when callbacks are available)
        // Not a blocker for Phase 4.

        // ── Phase 3: Build Metadata & Upsert ───────────────────────────
        // Persist `metadata.repo` as GitHub `owner/name` for gh gateway.

        let metadata = json!({
            "repo": repo,
            "worktreeRoot": input.worktree_root,
            "normalizedDerivedId": input.id_source == "derived",
            "source": "api",
        });

        let record = ProjectRecord {
            id: normalized_id.clone(),
            name: input.name,
            repo_path: input.repo_path,
            base_branch: Some(input.base_branch),
            archived: false,
            metadata_json: Some(metadata.to_string()),
            created_at: existing.as_ref().map(|e| e.created_at.clone()).unwrap_or(now_iso.clone()),
            updated_at: now_iso,
        };

        self.repos.projects.upsert(&record)?;

        if let Some(ref existing_rec) = existing {
            if existing_rec.archived {
                info!(project_id = %record.id, "reactivated archived project");
            }
        }

        // ── Phase 4: Discovery ─────────────────────────────────────────
        let snapshot_mode = input.snapshot_mode;

        let discovered_worktrees = self.discover_worktrees_internal(&record, &mut warnings)?;

        let (discovered_prs, pending_snapshots, captured_snapshots) =
            self.discover_pull_requests_internal(&record, repo.as_deref(), snapshot_mode, &mut warnings)?;

        // Event log
        let _ = append(
            &self.repos.events,
            &AppendInput {
                event_type: "project.added".into(),
                project_id: Some(record.id.clone()),
                payload_json: Some(
                    json!({
                        "id": record.id,
                        "repo": repo,
                        "repoPath": record.repo_path,
                    })
                    .to_string(),
                ),
                ..AppendInput::new("")
            },
        );

        info!(
            project_id = %record.id,
            repo = ?repo,
            worktrees = discovered_worktrees,
            prs = discovered_prs,
            "project added"
        );

        Ok(AddResult {
            project: record,
            repo,
            discovered_pull_requests: discovered_prs,
            discovered_worktrees,
            pending_snapshots,
            captured_snapshots,
            warnings,
        })
    }

    // ── RemoveProject ───────────────────────────────────────────────────

    pub fn remove_project(&self, identifier: &str) -> Result<ProjectRecord> {
        let now = (self.now)();
        let now_iso = now.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();

        if identifier.is_empty() {
            return Err(ServiceError::InvalidProjectID("identifier is empty".into()));
        }

        // 1. Resolve project
        let project = self.repos.projects.get_by_id(identifier)?.filter(|p| !p.archived);

        let project = match project {
            Some(p) => p,
            None => {
                // Try name match
                let all = self.repos.projects.list()?;
                let matches: Vec<_> = all
                    .iter()
                    .filter(|p| !p.archived && p.name.to_lowercase().trim() == identifier.to_lowercase().trim())
                    .collect();

                if matches.len() == 1 {
                    matches[0].clone()
                } else if matches.len() > 1 {
                    return Err(ServiceError::AmbiguousProjectIdentifier(identifier.to_string()));
                } else {
                    return Err(ServiceError::ProjectNotFound(identifier.to_string()));
                }
            }
        };

        // 2. Reject config-managed projects
        if let Some(ref meta_json) = project.metadata_json {
            if let Ok(meta) = serde_json::from_str::<serde_json::Value>(meta_json) {
                if meta.get("source").and_then(|s| s.as_str()) == Some("config") {
                    return Err(ServiceError::ConfigManagedProject(project.id.clone()));
                }
            }
        }

        // 3. Archive project
        let archived = self.repos.projects.archive(&project.id, &now_iso)?;

        if !archived {
            return Err(ServiceError::ProjectNotFound(project.id.clone()));
        }

        // 4. Terminate active loops
        self.repos.loops.terminate_by_project(&project.id, &now_iso)?;

        // 5. Cancel queue items
        self.repos.queue.cancel_by_project(&project.id, &now_iso, Some("project archived"))?;

        let mut archived_project = project;
        archived_project.archived = true;
        archived_project.updated_at = now_iso;

        info!(project_id = %archived_project.id, "project removed");

        Ok(archived_project)
    }

    // ── List ────────────────────────────────────────────────────────────

    pub fn list(&self) -> Result<Vec<ProjectRecord>> {
        let projects = self.repos.projects.list()?;
        let mut active: Vec<ProjectRecord> = projects.into_iter().filter(|p| !p.archived).collect();
        active.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(active)
    }

    // ── UpdateProject ───────────────────────────────────────────────────

    /// Patch mutable project fields and persist via upsert.
    ///
    /// Supported mutations: `default_branch` (base_branch), `enabled` (archived),
    /// `schedule` / `archive_filter` (stored in `metadata_json`), `path` (repo_path),
    /// `repo` (metadata.repo — explicit owner/name; re-detect when path changes).
    pub fn update_project(&self, identifier: &str, input: UpdateInput) -> Result<ProjectRecord> {
        let now = (self.now)();
        let now_iso = now.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();

        if identifier.is_empty() {
            return Err(ServiceError::InvalidProjectID("identifier is empty".into()));
        }

        // Resolve by id first, then by name (including archived so we can re-enable).
        let project = match self.repos.projects.get_by_id(identifier)? {
            Some(p) => p,
            None => {
                let all = self.repos.projects.list()?;
                let matches: Vec<_> =
                    all.iter().filter(|p| p.name.to_lowercase().trim() == identifier.to_lowercase().trim()).collect();
                if matches.len() == 1 {
                    matches[0].clone()
                } else if matches.len() > 1 {
                    return Err(ServiceError::AmbiguousProjectIdentifier(identifier.to_string()));
                } else {
                    return Err(ServiceError::ProjectNotFound(identifier.to_string()));
                }
            }
        };

        let path_changed = input.path.as_ref().is_some_and(|p| p != &project.repo_path);
        let mut updated = project;
        if let Some(branch) = input.default_branch {
            updated.base_branch = Some(branch);
        }
        if let Some(path) = input.path {
            updated.repo_path = path;
        }
        if let Some(enabled) = input.enabled {
            updated.archived = !enabled;
        }

        // Resolve metadata.repo: explicit repo wins; else re-detect when path changes.
        let explicit_repo = input.repo.as_ref().and_then(|r| {
            let t = r.trim();
            if t.is_empty() {
                None
            } else {
                Some(normalize_repo_spec(t))
            }
        });
        let detected_repo = if explicit_repo.is_none() && path_changed {
            if let Some(ref detect_fn) = self.callbacks.detect_repo {
                match detect_fn(&updated.repo_path) {
                    Ok(Some(r)) => Some(normalize_repo_spec(&r)),
                    Ok(None) => {
                        warn!(
                            project_id = %updated.id,
                            "update: could not re-detect GitHub repo after path change"
                        );
                        None
                    }
                    Err(e) => {
                        warn!(
                            project_id = %updated.id,
                            "update: detect_repo failed after path change: {e}"
                        );
                        None
                    }
                }
            } else {
                None
            }
        } else {
            None
        };
        let new_repo = explicit_repo.or(detected_repo);

        // Merge schedule / archive_filter / repo into metadata_json without dropping keys.
        let needs_meta_merge = input.schedule.is_some() || input.archive_filter.is_some() || new_repo.is_some();
        if needs_meta_merge {
            let mut meta = updated
                .metadata_json
                .as_deref()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                .unwrap_or_else(|| json!({}));
            if !meta.is_object() {
                meta = json!({});
            }
            if let Some(schedule) = input.schedule {
                meta["schedule"] = json!(schedule);
            }
            if let Some(filter) = input.archive_filter {
                meta["archive_filter"] = json!(filter);
            }
            if let Some(repo) = new_repo {
                meta["repo"] = json!(repo);
            }
            updated.metadata_json = Some(meta.to_string());
        }

        updated.updated_at = now_iso;
        self.repos.projects.upsert(&updated)?;

        info!(project_id = %updated.id, "project updated");
        Ok(updated)
    }

    // ── SyncConfigured ──────────────────────────────────────────────────

    pub fn sync_configured(&self, cfg: &Config, now: DateTime<Utc>) -> Result<()> {
        let now_iso = now.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();

        let project_configs = &cfg.projects[..];

        for proj_cfg in project_configs {
            if !proj_cfg.enabled {
                continue;
            }

            // Derive project ID from name
            let project_id: String = proj_cfg
                .name
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
                .collect();

            let repo_path = proj_cfg.path.as_deref().unwrap_or(&proj_cfg.name).to_string();

            // Check existing
            let existing = self.repos.projects.get_by_id(&project_id)?;

            // Detect repo — prefer fresh detect; preserve existing metadata.repo on failure.
            let existing_repo = existing.as_ref().and_then(|ex| repo_from_metadata(ex.metadata_json.as_deref()));
            let repo = if let Some(ref detect_fn) = self.callbacks.detect_repo {
                match detect_fn(&repo_path) {
                    Ok(Some(r)) => Some(normalize_repo_spec(&r)),
                    Ok(None) => {
                        if existing_repo.is_none() {
                            warn!(
                                project_id = %project_id,
                                "could not detect repo from git remote and no existing fallback"
                            );
                        }
                        existing_repo
                    }
                    Err(e) => {
                        if existing_repo.is_none() {
                            warn!(
                                project_id = %project_id,
                                "could not detect repo for config project: {e}"
                            );
                        }
                        existing_repo
                    }
                }
            } else {
                existing_repo
            };

            let metadata = json!({
                "repo": repo,
                "source": "config",
            });

            let record = ProjectRecord {
                id: project_id.clone(),
                name: proj_cfg.name.clone(),
                repo_path,
                base_branch: None, // use None — callers can set it later
                archived: false,
                metadata_json: Some(metadata.to_string()),
                created_at: existing.as_ref().map(|e| e.created_at.clone()).unwrap_or(now_iso.clone()),
                updated_at: now_iso.clone(),
            };

            self.repos.projects.upsert(&record)?;
        }

        Ok(())
    }

    // ── Internal helpers ────────────────────────────────────────────────

    fn discover_worktrees_internal(&self, project: &ProjectRecord, warnings: &mut Vec<String>) -> Result<usize> {
        let now = (self.now)();
        let now_iso = now.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();

        let list_worktrees = match self.callbacks.list_worktrees {
            Some(ref f) => f,
            None => return Ok(0),
        };

        let entries = match list_worktrees(&project.repo_path) {
            Ok(e) => e,
            Err(e) => {
                warnings.push(format!("Could not discover worktrees: {e}"));
                return Ok(0);
            }
        };

        let mut count = 0usize;
        for entry in entries {
            if entry.branch.is_empty() {
                continue;
            }

            // Check existing worktree by (project_id, branch)
            let existing = self.repos.worktrees.get_by_branch(&project.id, &entry.branch)?;
            let base_branch = existing
                .as_ref()
                .and_then(|w| w.base_branch.clone())
                .or_else(|| project.base_branch.clone())
                .unwrap_or_else(|| entry.branch.clone());

            let head_sha = entry.head_sha.clone().or_else(|| existing.as_ref().and_then(|w| w.head_sha.clone()));

            let wt_id =
                existing.as_ref().map(|w| w.id.clone()).unwrap_or_else(|| looper_storage::eventlog::new_event_id("wt"));

            let wt_record = WorktreeRecord {
                id: wt_id,
                project_id: project.id.clone(),
                repo_path: project.repo_path.clone(),
                worktree_path: entry.worktree_path,
                branch: entry.branch,
                base_branch: Some(base_branch),
                status: "active".into(),
                head_sha,
                metadata_json: None,
                created_at: existing.as_ref().map(|w| w.created_at.clone()).unwrap_or(now_iso.clone()),
                updated_at: now_iso.clone(),
                cleaned_at: None,
            };

            self.repos.worktrees.upsert(&wt_record)?;
            count += 1;
        }

        Ok(count)
    }

    fn discover_pull_requests_internal(
        &self,
        project: &ProjectRecord,
        repo: Option<&str>,
        mode: SnapshotMode,
        warnings: &mut Vec<String>,
    ) -> Result<(usize, usize, usize)> {
        let now = (self.now)();
        let now_iso = now.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
        let repo = match repo {
            Some(r) => r,
            None => return Ok((0, 0, 0)),
        };

        if mode == SnapshotMode::Off {
            return Ok((0, 0, 0));
        }

        let list_prs = match self.callbacks.list_open_pull_requests {
            Some(ref f) => f,
            None => return Ok((0, 0, 0)),
        };

        // Determine actual mode
        let actual_mode = if mode == SnapshotMode::Async {
            // For now, async mode without a scheduler falls back to full
            SnapshotMode::Full
        } else {
            mode
        };

        let entries = match list_prs(repo, &project.repo_path) {
            Ok(e) => e,
            Err(e) => {
                warnings.push(format!("Could not discover pull requests: {e}"));
                return Ok((0, 0, 0));
            }
        };

        // Filter: non-draft, open
        let open_prs: Vec<_> = entries.into_iter().filter(|pr| !pr.draft && pr.state == "open").collect();

        let discovered = open_prs.len();
        let mut pending = 0usize;
        let mut captured = 0usize;

        match actual_mode {
            SnapshotMode::Full => {
                if let Some(ref snap_fn) = self.callbacks.capture_pull_request_snapshot {
                    for pr in &open_prs {
                        match snap_fn(&project.id, repo, pr.number, &project.repo_path) {
                            Ok(snapshot) => {
                                self.repos.pull_request_snapshots.upsert(&snapshot)?;
                                captured += 1;
                            }
                            Err(e) => {
                                warnings.push(format!("Could not snapshot PR #{}: {e}", pr.number));
                            }
                        }
                    }
                }
            }
            SnapshotMode::Async => {
                // Async mode: enqueue snapshot jobs
                for pr in &open_prs {
                    let dedupe_key = format!("snapshot:{}:{}:{}", project.id, repo, pr.number);

                    // Build queue payload
                    let payload = json!({
                        "projectId": project.id,
                        "repo": repo,
                        "prNumber": pr.number,
                        "headSha": pr.head_sha,
                    });

                    // Check for existing active queue item
                    let existing_queue = self.repos.queue.find_active_by_dedupe(&dedupe_key)?;
                    if existing_queue.is_none() {
                        pending += 1;
                        self.repos.queue.create_or_get_active_by_dedupe(&looper_storage::record::QueueItemRecord {
                            id: looper_storage::eventlog::new_event_id("q"),
                            project_id: Some(project.id.clone()),
                            loop_id: None,
                            r#type: "snapshot".into(),
                            target_type: "pull_request".into(),
                            target_id: pr.number.to_string(),
                            repo: Some(repo.to_string()),
                            pr_number: Some(pr.number),
                            dedupe_key,
                            priority: 10, // QueuePrioritySnapshot
                            status: "queued".into(),
                            available_at: now_iso.clone(),
                            attempts: 0,
                            max_attempts: 3,
                            claimed_by: None,
                            claimed_at: None,
                            started_at: None,
                            finished_at: None,
                            lock_key: None,
                            payload_json: Some(payload.to_string()),
                            last_error: None,
                            last_error_kind: None,
                            created_at: now_iso.clone(),
                            updated_at: now_iso.clone(),
                        })?;
                    }
                }
            }
            SnapshotMode::Off => {}
        }

        Ok((discovered, pending, captured))
    }
}

/// ── Snapshot mode ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotMode {
    Async,
    Full,
    Off,
}

/// ── Input / Output types ────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct AddInput {
    pub id: String,
    pub name: String,
    pub repo_path: String,
    pub base_branch: String,
    pub id_source: String, // "explicit" | "derived"
    pub worktree_root: Option<String>,
    pub repo: Option<String>,
    pub snapshot_mode: SnapshotMode,
}

/// Partial update for an existing project. All fields are optional.
#[derive(Debug, Clone, Default)]
pub struct UpdateInput {
    pub schedule: Option<String>,
    pub enabled: Option<bool>,
    pub archive_filter: Option<String>,
    pub default_branch: Option<String>,
    pub path: Option<String>,
    /// Explicit GitHub `owner/name` (or URL) → stored as `metadata.repo`.
    pub repo: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AddResult {
    pub project: ProjectRecord,
    pub repo: Option<String>,
    pub discovered_pull_requests: usize,
    pub discovered_worktrees: usize,
    pub pending_snapshots: usize,
    pub captured_snapshots: usize,
    pub warnings: Vec<String>,
}

// ── Repo resolution (shared for admit-work + discovery) ─────────────────

/// Resolve the GitHub `owner/name` for a project.
///
/// Canonical sources (in order):
/// 1. `metadata_json.repo` — preferred; set by detect_repo or explicit `--repo-url`
/// 2. `repo_path` if it already looks like `owner/name` (not a filesystem path)
///
/// Callers that require a repo for GitHub gateway operations (admit-work,
/// discovery) should use this and fail fast on error rather than silently
/// skipping.
pub fn resolve_project_repo(project: &ProjectRecord) -> Result<String> {
    if let Some(repo) = repo_from_metadata(project.metadata_json.as_deref()) {
        return Ok(repo);
    }
    let rp = project.repo_path.trim();
    if looks_like_owner_repo(rp) {
        return Ok(rp.to_string());
    }
    Err(ServiceError::ProjectRepoUnresolved(format!(
        "project '{}' has no resolvable GitHub repo (owner/name). \
         Set it with `looper projects add --repo-url owner/name` (or pass repo_url on the API), \
         or ensure local path '{}' is a git checkout whose `remote.origin.url` is a github.com remote \
         so auto-detect can populate metadata.repo. \
         Without this, admit-work and discovery cannot call the GitHub gateway.",
        project.id, project.repo_path
    )))
}

/// Best-effort effective repo for list/GET surfaces (no error).
pub fn effective_project_repo(project: &ProjectRecord) -> Option<String> {
    resolve_project_repo(project).ok()
}

/// Extract and normalize `metadata.repo` from a project's metadata_json.
pub fn repo_from_metadata(metadata_json: Option<&str>) -> Option<String> {
    let raw = metadata_json?;
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let repo = v.get("repo")?;
    match repo {
        serde_json::Value::String(s) => {
            let s = s.trim();
            if s.is_empty() {
                None
            } else {
                Some(normalize_repo_spec(s))
            }
        }
        _ => None,
    }
}

/// Normalize a user-supplied or detected repo to `owner/name` when possible.
///
/// Accepts:
/// - `owner/name`
/// - `https://github.com/owner/name(.git)`
/// - `git@github.com:owner/name(.git)`
pub fn normalize_repo_spec(repo: &str) -> String {
    let mut s = repo.trim().to_string();
    if s.ends_with('/') {
        s.pop();
    }
    if let Some(rest) = s.strip_prefix("https://github.com/") {
        s = rest.to_string();
    } else if let Some(rest) = s.strip_prefix("http://github.com/") {
        s = rest.to_string();
    } else if let Some(rest) = s.strip_prefix("git@github.com:") {
        s = rest.to_string();
    }
    if let Some(stripped) = s.strip_suffix(".git") {
        s = stripped.to_string();
    }
    // Drop trailing path segments beyond owner/name (e.g. /pulls).
    let parts: Vec<&str> = s.split('/').filter(|p| !p.is_empty()).collect();
    if parts.len() >= 2 {
        format!("{}/{}", parts[0], parts[1])
    } else {
        s
    }
}

/// True when `s` looks like a GitHub `owner/name` slug (not a filesystem path).
fn looks_like_owner_repo(s: &str) -> bool {
    let s = s.trim();
    if s.is_empty() || s.starts_with('/') || s.starts_with('.') || s.contains('\\') || s.contains(':') {
        return false;
    }
    let parts: Vec<&str> = s.split('/').collect();
    if parts.len() != 2 {
        return false;
    }
    parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
}

/// Resolve repo for AddProject: explicit input wins; else detect_repo callback.
fn resolve_repo_for_add(
    input: &AddInput,
    callbacks: &ProjectServiceCallbacks,
    warnings: &mut Vec<String>,
) -> Option<String> {
    if let Some(ref explicit) = input.repo {
        let trimmed = explicit.trim();
        if !trimmed.is_empty() {
            return Some(normalize_repo_spec(trimmed));
        }
    }

    if let Some(ref detect_fn) = callbacks.detect_repo {
        return match detect_fn(&input.repo_path) {
            Ok(Some(r)) => {
                let normalized = normalize_repo_spec(&r);
                if normalized.is_empty() {
                    warnings.push("Could not detect GitHub repo from git remote".into());
                    None
                } else {
                    Some(normalized)
                }
            }
            Ok(None) => {
                warnings
                    .push("Could not detect GitHub repo from git remote — set --repo-url owner/name explicitly".into());
                None
            }
            Err(e) => {
                warnings.push(format!("Could not detect GitHub repo: {e}. Set --repo-url owner/name explicitly"));
                None
            }
        };
    }

    None
}

// ── Validation ──────────────────────────────────────────────────────────

fn validate_project_id(id: &str) -> Result<()> {
    if id.is_empty() || id == "." || id == ".." {
        return Err(ServiceError::InvalidProjectID(format!("invalid project ID: '{id}'")));
    }
    if id.contains('/') || id.contains('\\') {
        return Err(ServiceError::InvalidProjectID(format!("project ID must not contain path separators: '{id}'")));
    }
    if id.starts_with('/') || id.starts_with('\\') {
        return Err(ServiceError::InvalidProjectID(format!("project ID must not be an absolute path: '{id}'")));
    }
    if id.starts_with("legacy-id-") {
        return Err(ServiceError::InvalidProjectID(format!("project ID must not start with 'legacy-id-': '{id}'")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::Utc;
    use rusqlite::Connection;

    use looper_storage::migration::run_migrations;
    use looper_storage::repos::Repositories;

    use super::*;

    fn repos_setup() -> Arc<Repositories> {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;").unwrap();
        run_migrations(&mut conn).unwrap();
        Arc::new(Repositories::new(conn))
    }

    fn svc(repos: Arc<Repositories>) -> ProjectService {
        ProjectService::new(repos, ProjectServiceCallbacks::new(), Utc::now)
    }

    // ── Struct tests ─────────────────────────────────────────────────

    #[test]
    fn test_add_input_phases() {
        let input = AddInput {
            id: "my-project".into(),
            name: "My Project".into(),
            repo_path: "/tmp/repo".into(),
            base_branch: "main".into(),
            id_source: "explicit".into(),
            worktree_root: None,
            repo: None,
            snapshot_mode: SnapshotMode::Async,
        };
        assert_eq!(input.id, "my-project");
        assert_eq!(input.name, "My Project");
    }

    #[test]
    fn test_pull_request_entry() {
        let entry = PullRequestEntry {
            number: 42,
            title: Some("feat: widget".into()),
            body: None,
            author: Some("user1".into()),
            head_sha: "abc123".into(),
            base_sha: Some("def456".into()),
            draft: false,
            state: "open".into(),
        };
        assert_eq!(entry.number, 42);
        assert_eq!(entry.title.as_deref(), Some("feat: widget"));
    }

    #[test]
    fn test_worktree_entry() {
        let entry = WorktreeEntry {
            branch: "feat/widget".into(),
            worktree_path: "/tmp/worktrees/feat-widget".into(),
            head_sha: None,
        };
        assert_eq!(entry.branch, "feat/widget");
        assert_eq!(entry.worktree_path, "/tmp/worktrees/feat-widget");
    }

    // ── Validation ───────────────────────────────────────────────────

    #[test]
    fn test_validate_empty_id() {
        assert!(validate_project_id("").is_err());
        assert!(validate_project_id(".").is_err());
        assert!(validate_project_id("..").is_err());
    }

    #[test]
    fn test_validate_separator_in_id() {
        assert!(validate_project_id("a/b").is_err());
        assert!(validate_project_id("a\\b").is_err());
        assert!(validate_project_id("/abs").is_err());
    }

    #[test]
    fn test_validate_legacy_prefix() {
        assert!(validate_project_id("legacy-id-xxx").is_err());
    }

    #[test]
    fn test_validate_good_id() {
        assert!(validate_project_id("my-project").is_ok());
        assert!(validate_project_id("hello123").is_ok());
    }

    // ── Business-logic tests ─────────────────────────────────────────

    #[test]
    fn test_add_project_success() {
        let repos = repos_setup();
        let s = svc(repos.clone());
        let result = s
            .add_project(AddInput {
                id: "my-proj".into(),
                name: "My Project".into(),
                repo_path: "/tmp/p".into(),
                base_branch: "main".into(),
                id_source: "explicit".into(),
                worktree_root: None,
                repo: None,
                snapshot_mode: SnapshotMode::Off,
            })
            .unwrap();
        assert_eq!(result.project.id, "my-proj");
        assert!(!result.project.archived);
    }

    #[test]
    fn test_add_project_empty_id_rejected() {
        let repos = repos_setup();
        let s = svc(repos);
        let err = s
            .add_project(AddInput {
                id: "".into(),
                name: "X".into(),
                repo_path: "/tmp/p".into(),
                base_branch: "main".into(),
                id_source: "explicit".into(),
                worktree_root: None,
                repo: None,
                snapshot_mode: SnapshotMode::Off,
            })
            .unwrap_err();
        assert!(matches!(err, ServiceError::InvalidProjectID(_)));
    }

    #[test]
    fn test_add_project_id_collision() {
        let repos = repos_setup();
        let s = svc(repos.clone());
        s.add_project(AddInput {
            id: "proj".into(),
            name: "P".into(),
            repo_path: "/tmp/p".into(),
            base_branch: "main".into(),
            id_source: "explicit".into(),
            worktree_root: None,
            repo: None,
            snapshot_mode: SnapshotMode::Off,
        })
        .unwrap();
        let err = s
            .add_project(AddInput {
                id: "proj".into(),
                name: "P".into(),
                repo_path: "/tmp/p".into(),
                base_branch: "main".into(),
                id_source: "explicit".into(),
                worktree_root: None,
                repo: None,
                snapshot_mode: SnapshotMode::Off,
            })
            .unwrap_err();
        assert!(matches!(err, ServiceError::ProjectIDCollision(_)));
    }

    #[test]
    fn test_add_derived_id_normalizes() {
        let repos = repos_setup();
        let s = svc(repos.clone());
        let result = s
            .add_project(AddInput {
                id: "My Cool Repo!!".into(),
                name: "X".into(),
                repo_path: "/tmp/p".into(),
                base_branch: "main".into(),
                id_source: "derived".into(),
                worktree_root: None,
                repo: None,
                snapshot_mode: SnapshotMode::Off,
            })
            .unwrap();
        assert_eq!(result.project.id, "my-cool-repo");
    }

    #[test]
    fn test_remove_project() {
        let repos = repos_setup();
        let s = svc(repos.clone());
        s.add_project(AddInput {
            id: "proj".into(),
            name: "P".into(),
            repo_path: "/tmp/p".into(),
            base_branch: "main".into(),
            id_source: "explicit".into(),
            worktree_root: None,
            repo: None,
            snapshot_mode: SnapshotMode::Off,
        })
        .unwrap();
        let removed = s.remove_project("proj").unwrap();
        assert!(removed.archived);
        // After removal, list should be empty
        assert!(s.list().unwrap().is_empty());
    }

    #[test]
    fn test_remove_nonexistent_project() {
        let repos = repos_setup();
        let s = svc(repos);
        let err = s.remove_project("nope").unwrap_err();
        assert!(matches!(err, ServiceError::ProjectNotFound(_)));
    }

    #[test]
    fn test_list_projects() {
        let repos = repos_setup();
        let s = svc(repos.clone());
        s.add_project(AddInput {
            id: "a".into(),
            name: "A".into(),
            repo_path: "/tmp/a".into(),
            base_branch: "main".into(),
            id_source: "explicit".into(),
            worktree_root: None,
            repo: None,
            snapshot_mode: SnapshotMode::Off,
        })
        .unwrap();
        s.add_project(AddInput {
            id: "b".into(),
            name: "B".into(),
            repo_path: "/tmp/b".into(),
            base_branch: "main".into(),
            id_source: "explicit".into(),
            worktree_root: None,
            repo: None,
            snapshot_mode: SnapshotMode::Off,
        })
        .unwrap();
        let projects = s.list().unwrap();
        assert_eq!(projects.len(), 2);
    }

    #[test]
    fn test_remove_unknown_project_returns_not_found() {
        let repos = repos_setup();
        let s = svc(repos);
        let err = s.remove_project("does-not-exist").unwrap_err();
        match &err {
            ServiceError::ProjectNotFound(id) => assert_eq!(id, "does-not-exist"),
            _ => panic!("expected ProjectNotFound, got {err:?}"),
        }
    }

    #[test]
    fn test_update_project_persists_mutations() {
        let repos = repos_setup();
        let s = svc(repos.clone());
        s.add_project(AddInput {
            id: "upd".into(),
            name: "Upd".into(),
            repo_path: "/tmp/upd".into(),
            base_branch: "main".into(),
            id_source: "explicit".into(),
            worktree_root: None,
            repo: Some("org/upd".into()),
            snapshot_mode: SnapshotMode::Off,
        })
        .unwrap();

        let updated = s
            .update_project(
                "upd",
                UpdateInput {
                    schedule: Some("0 * * * *".into()),
                    enabled: Some(true),
                    archive_filter: Some("merged".into()),
                    default_branch: Some("develop".into()),
                    path: Some("/tmp/upd-new".into()),
                    repo: None,
                },
            )
            .unwrap();

        assert_eq!(updated.base_branch.as_deref(), Some("develop"));
        assert_eq!(updated.repo_path, "/tmp/upd-new");
        assert!(!updated.archived);

        let meta: serde_json::Value = serde_json::from_str(updated.metadata_json.as_deref().unwrap()).unwrap();
        assert_eq!(meta["schedule"], "0 * * * *");
        assert_eq!(meta["archive_filter"], "merged");
        // Existing repo key preserved when path changes without detect/explicit
        assert_eq!(meta["repo"], "org/upd");

        // Reload from storage — GET should reflect PUT
        let reloaded = repos.projects.get_by_id("upd").unwrap().unwrap();
        assert_eq!(reloaded.base_branch.as_deref(), Some("develop"));
        assert_eq!(reloaded.repo_path, "/tmp/upd-new");
        let meta2: serde_json::Value = serde_json::from_str(reloaded.metadata_json.as_deref().unwrap()).unwrap();
        assert_eq!(meta2["schedule"], "0 * * * *");
        assert_eq!(meta2["archive_filter"], "merged");
    }

    #[test]
    fn test_update_project_disable_archives() {
        let repos = repos_setup();
        let s = svc(repos.clone());
        s.add_project(AddInput {
            id: "dis".into(),
            name: "Dis".into(),
            repo_path: "/tmp/dis".into(),
            base_branch: "main".into(),
            id_source: "explicit".into(),
            worktree_root: None,
            repo: None,
            snapshot_mode: SnapshotMode::Off,
        })
        .unwrap();

        let updated = s.update_project("dis", UpdateInput { enabled: Some(false), ..Default::default() }).unwrap();
        assert!(updated.archived);
        assert!(s.list().unwrap().is_empty());

        // Re-enable
        let reenabled = s.update_project("dis", UpdateInput { enabled: Some(true), ..Default::default() }).unwrap();
        assert!(!reenabled.archived);
        assert_eq!(s.list().unwrap().len(), 1);
    }

    #[test]
    fn test_remove_empty_id() {
        let repos = repos_setup();
        let s = svc(repos);
        let err = s.remove_project("").unwrap_err();
        assert!(matches!(err, ServiceError::InvalidProjectID(_)));
    }

    // ── Repo metadata contract ───────────────────────────────────────

    #[test]
    fn test_detect_repo_mock_sets_metadata_repo() {
        let repos = repos_setup();
        let mut callbacks = ProjectServiceCallbacks::new();
        callbacks.detect_repo = Some(Arc::new(|path: &str| {
            assert_eq!(path, "/tmp/my-checkout");
            Ok(Some("acme/widget".into()))
        }));
        let s = ProjectService::new(repos.clone(), callbacks, Utc::now);
        let result = s
            .add_project(AddInput {
                id: "widget".into(),
                name: "Widget".into(),
                repo_path: "/tmp/my-checkout".into(),
                base_branch: "main".into(),
                id_source: "explicit".into(),
                worktree_root: None,
                repo: None,
                snapshot_mode: SnapshotMode::Off,
            })
            .unwrap();

        assert_eq!(result.repo.as_deref(), Some("acme/widget"));
        let stored = repos.projects.get_by_id("widget").unwrap().unwrap();
        assert_eq!(repo_from_metadata(stored.metadata_json.as_deref()).as_deref(), Some("acme/widget"));
        assert_eq!(resolve_project_repo(&stored).unwrap(), "acme/widget");
    }

    #[test]
    fn test_explicit_repo_wins_over_detect() {
        let repos = repos_setup();
        let mut callbacks = ProjectServiceCallbacks::new();
        callbacks.detect_repo = Some(Arc::new(|_: &str| Ok(Some("detected/wrong".into()))));
        let s = ProjectService::new(repos.clone(), callbacks, Utc::now);
        let result = s
            .add_project(AddInput {
                id: "proj".into(),
                name: "P".into(),
                repo_path: "/tmp/p".into(),
                base_branch: "main".into(),
                id_source: "explicit".into(),
                worktree_root: None,
                repo: Some("https://github.com/acme/explicit.git".into()),
                snapshot_mode: SnapshotMode::Off,
            })
            .unwrap();
        assert_eq!(result.repo.as_deref(), Some("acme/explicit"));
        let stored = repos.projects.get_by_id("proj").unwrap().unwrap();
        assert_eq!(resolve_project_repo(&stored).unwrap(), "acme/explicit");
    }

    #[test]
    fn test_resolve_project_repo_without_repo_errors_cleanly() {
        let project = ProjectRecord {
            id: "lonely".into(),
            name: "Lonely".into(),
            repo_path: "/tmp/no-remote".into(),
            base_branch: Some("main".into()),
            archived: false,
            metadata_json: Some(r#"{"repo":null,"source":"api"}"#.into()),
            created_at: "2026-01-01T00:00:00.000Z".into(),
            updated_at: "2026-01-01T00:00:00.000Z".into(),
        };
        let err = resolve_project_repo(&project).unwrap_err();
        match err {
            ServiceError::ProjectRepoUnresolved(msg) => {
                assert!(msg.contains("lonely"), "message should name the project: {msg}");
                assert!(
                    msg.contains("repo-url") || msg.contains("metadata.repo") || msg.contains("owner/name"),
                    "message should be actionable: {msg}"
                );
            }
            other => panic!("expected ProjectRepoUnresolved, got {other:?}"),
        }
        assert!(effective_project_repo(&project).is_none());
    }

    #[test]
    fn test_resolve_project_repo_falls_back_to_owner_name_path() {
        let project = ProjectRecord {
            id: "slug".into(),
            name: "Slug".into(),
            repo_path: "acme/slug-repo".into(),
            base_branch: None,
            archived: false,
            metadata_json: None,
            created_at: "2026-01-01T00:00:00.000Z".into(),
            updated_at: "2026-01-01T00:00:00.000Z".into(),
        };
        assert_eq!(resolve_project_repo(&project).unwrap(), "acme/slug-repo");
    }

    #[test]
    fn test_normalize_repo_spec_variants() {
        assert_eq!(normalize_repo_spec("owner/name"), "owner/name");
        assert_eq!(normalize_repo_spec("https://github.com/owner/name.git"), "owner/name");
        assert_eq!(normalize_repo_spec("git@github.com:owner/name.git"), "owner/name");
        assert_eq!(normalize_repo_spec("  owner/name  "), "owner/name");
    }

    #[test]
    fn test_update_project_explicit_repo_sets_metadata() {
        let repos = repos_setup();
        let s = svc(repos.clone());
        s.add_project(AddInput {
            id: "repo-upd".into(),
            name: "RepoUpd".into(),
            repo_path: "/tmp/repo-upd".into(),
            base_branch: "main".into(),
            id_source: "explicit".into(),
            worktree_root: None,
            repo: None,
            snapshot_mode: SnapshotMode::Off,
        })
        .unwrap();

        let updated = s
            .update_project(
                "repo-upd",
                UpdateInput { repo: Some("https://github.com/acme/updated.git".into()), ..Default::default() },
            )
            .unwrap();
        assert_eq!(repo_from_metadata(updated.metadata_json.as_deref()).as_deref(), Some("acme/updated"));
        assert_eq!(resolve_project_repo(&updated).unwrap(), "acme/updated");
    }

    #[test]
    fn test_update_path_redetects_repo() {
        let repos = repos_setup();
        let mut callbacks = ProjectServiceCallbacks::new();
        callbacks.detect_repo =
            Some(Arc::new(
                |path: &str| {
                    if path == "/tmp/new-checkout" {
                        Ok(Some("acme/new-repo".into()))
                    } else {
                        Ok(None)
                    }
                },
            ));
        let s = ProjectService::new(repos.clone(), callbacks, Utc::now);
        s.add_project(AddInput {
            id: "repath".into(),
            name: "Repath".into(),
            repo_path: "/tmp/old".into(),
            base_branch: "main".into(),
            id_source: "explicit".into(),
            worktree_root: None,
            repo: Some("acme/old-repo".into()),
            snapshot_mode: SnapshotMode::Off,
        })
        .unwrap();

        let updated = s
            .update_project("repath", UpdateInput { path: Some("/tmp/new-checkout".into()), ..Default::default() })
            .unwrap();
        assert_eq!(updated.repo_path, "/tmp/new-checkout");
        assert_eq!(repo_from_metadata(updated.metadata_json.as_deref()).as_deref(), Some("acme/new-repo"));
    }
}
