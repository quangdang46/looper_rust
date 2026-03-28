
use super::*;
use grove_types::{BeadPriority, HandoffRecord, Timestamp};
use serde_json::json;
use std::{collections::BTreeMap, error::Error, io::Error as IoError};

type TestResult = Result<(), Box<dyn Error>>;

#[derive(Default)]
struct FakeStore {
    beads: BTreeMap<String, BrIssueSummary>,
    dependencies: BTreeMap<String, (Vec<BeadId>, Vec<BeadId>)>,
    statuses: BTreeMap<String, GroveBeadStatus>,
}

impl FakeStore {
    fn with_status(mut self, bead_id: &str, status: GroveBeadStatus) -> Self {
        self.statuses.insert(bead_id.to_owned(), status);
        self
    }
}

impl BeadCacheStore for FakeStore {
    fn upsert_bead_cache(&mut self, bead: &BrIssueSummary) -> Result<UpsertOutcome> {
        let outcome = if self.beads.contains_key(bead.id.as_str()) {
            UpsertOutcome::Updated
        } else {
            UpsertOutcome::Added
        };
        self.beads.insert(bead.id.as_str().to_owned(), bead.clone());
        Ok(outcome)
    }

    fn replace_dependency_snapshot(
        &mut self,
        bead_id: &BeadId,
        blocked_by: &[BeadId],
        blocks: &[BeadId],
    ) -> Result<()> {
        self.dependencies.insert(
            bead_id.as_str().to_owned(),
            (blocked_by.to_vec(), blocks.to_vec()),
        );
        Ok(())
    }

    fn list_cached_beads(&self) -> Result<Vec<CachedBeadState>> {
        let mut ids: HashSet<String> = self.beads.keys().cloned().collect();
        ids.extend(self.statuses.keys().cloned());
        Ok(ids
            .into_iter()
            .map(|bead_id| CachedBeadState {
                bead_id: BeadId::new(bead_id.clone()),
                grove_status: self.statuses.get(&bead_id).copied(),
            })
            .collect())
    }

    fn set_grove_status(&mut self, bead_id: &BeadId, status: GroveBeadStatus) -> Result<()> {
        self.statuses.insert(bead_id.as_str().to_owned(), status);
        Ok(())
    }

    fn remove_bead_cache(&mut self, bead_id: &BeadId) -> Result<()> {
        self.beads.remove(bead_id.as_str());
        self.dependencies.remove(bead_id.as_str());
        self.statuses.remove(bead_id.as_str());
        Ok(())
    }
}

struct FakeBrClient {
    ready: Vec<BrIssueSummary>,
    list_open: Vec<BrIssueSummary>,
    dep_snapshots: BTreeMap<String, BrDependencySnapshot>,
    dep_failures: BTreeMap<String, String>,
}

impl BrClient for FakeBrClient {
    fn ready(&self) -> Result<Vec<BrIssueSummary>, BrError> {
        Ok(self.ready.clone())
    }

    fn list_open(&self) -> Result<Vec<BrIssueSummary>, BrError> {
        Ok(self.list_open.clone())
    }

    fn show(&self, id: &BeadId) -> Result<BrIssueDetail, BrError> {
        Err(BrError::BeadNotFound { id: id.clone() })
    }

    fn dep_list(&self, id: &BeadId) -> Result<BrDependencySnapshot, BrError> {
        if let Some(message) = self.dep_failures.get(id.as_str()) {
            return Err(BrError::ProtocolViolation {
                command: format!("br dep list {} --json", id),
                message: message.clone(),
                stdout: String::new(),
                stderr: String::new(),
            });
        }

        self.dep_snapshots
            .get(id.as_str())
            .cloned()
            .ok_or_else(|| BrError::ProtocolViolation {
                command: format!("br dep list {} --json", id),
                message: "missing fake dependency snapshot".into(),
                stdout: String::new(),
                stderr: String::new(),
            })
    }

    fn capability(&self) -> Result<BrCapability, BrError> {
        Ok(BrCapability {
            available: true,
            version_line: Some("br 0.1.12".into()),
            version: Some(BrVersion {
                raw: "br 0.1.12".into(),
                major: Some(0),
                minor: Some(1),
                patch: Some(12),
            }),
            beads_dir_exists: true,
        })
    }

    fn close_bead(&self, _id: &BeadId, _reason: Option<&str>) -> Result<(), BrError> {
        Ok(())
    }

    fn add_comment(&self, _id: &BeadId, _text: &str) -> Result<(), BrError> {
        Ok(())
    }

    fn mirror_handoff(
        &self,
        _id: &BeadId,
        _handoff: &HandoffRecord,
        _close_bead: bool,
    ) -> Result<(), BrError> {
        Ok(())
    }
}

#[test]
fn sync_bead_cache_upserts_open_beads_and_marks_ready_idle_beads() -> TestResult {
    let bead = sample_issue("grove-1j9.5.5", "grove-br", Vec::new(), Vec::new())?;
    let br = FakeBrClient {
        ready: vec![bead.clone()],
        list_open: vec![bead.clone()],
        dep_snapshots: BTreeMap::from([(
            bead.id.as_str().to_owned(),
            BrDependencySnapshot {
                bead_id: bead.id.clone(),
                blocked_by: vec![BeadId::new("grove-1j9.5.4")],
                blocks: vec![BeadId::new("grove-1j9.5.10")],
                rows: Vec::new(),
            },
        )]),
        dep_failures: BTreeMap::new(),
    };
    let mut store = FakeStore::default().with_status(bead.id.as_str(), GroveBeadStatus::Idle);

    let result = sync_bead_cache(&br, &mut store)?;

    assert_eq!(result.beads_synced, 1);
    assert_eq!(result.beads_added, 1);
    assert_eq!(result.dependencies_updated, 1);
    assert!(result.errors.is_empty());
    assert_eq!(
        store.statuses.get(bead.id.as_str()),
        Some(&GroveBeadStatus::Ready)
    );
    assert_eq!(
        store.dependencies.get(bead.id.as_str()),
        Some(&(
            vec![BeadId::new("grove-1j9.5.4")],
            vec![BeadId::new("grove-1j9.5.10")]
        )),
    );
    Ok(())
}

#[test]
fn sync_bead_cache_uses_inline_dependency_snapshot_when_present() -> TestResult {
    let bead = sample_issue(
        "grove-1j9.5.6",
        "grove-bv",
        vec![BeadId::new("grove-1j9.5.4")],
        vec![BeadId::new("grove-1j9.5.8")],
    )?;
    let br = FakeBrClient {
        ready: vec![bead.clone()],
        list_open: vec![bead.clone()],
        dep_snapshots: BTreeMap::new(),
        dep_failures: BTreeMap::from([(
            bead.id.as_str().to_owned(),
            "should not be called".into(),
        )]),
    };
    let mut store = FakeStore::default();

    let result = sync_bead_cache(&br, &mut store)?;

    assert_eq!(result.dependencies_updated, 1);
    assert!(result.errors.is_empty());
    assert_eq!(
        store.dependencies.get(bead.id.as_str()),
        Some(&(
            vec![BeadId::new("grove-1j9.5.4")],
            vec![BeadId::new("grove-1j9.5.8")]
        )),
    );
    Ok(())
}

#[test]
fn sync_bead_cache_counts_missing_non_running_cached_beads_as_removed() -> TestResult {
    let bead = sample_issue("grove-1j9.5.5", "grove-br", Vec::new(), Vec::new())?;
    let br = FakeBrClient {
        ready: vec![bead.clone()],
        list_open: vec![bead],
        dep_snapshots: BTreeMap::new(),
        dep_failures: BTreeMap::new(),
    };
    let mut store = FakeStore::default()
        .with_status("grove-old-idle", GroveBeadStatus::Idle)
        .with_status("grove-old-running", GroveBeadStatus::Running);

    let result = sync_bead_cache(&br, &mut store)?;

    assert_eq!(result.beads_removed, 1);
    Ok(())
}

#[test]
fn sync_bead_cache_collects_dependency_errors_and_continues() -> TestResult {
    let bead = sample_issue("grove-1j9.5.5", "grove-br", Vec::new(), Vec::new())?;
    let br = FakeBrClient {
        ready: vec![bead.clone()],
        list_open: vec![bead.clone()],
        dep_snapshots: BTreeMap::new(),
        dep_failures: BTreeMap::from([(bead.id.as_str().to_owned(), "boom".into())]),
    };
    let mut store = FakeStore::default();

    let result = sync_bead_cache(&br, &mut store)?;

    assert_eq!(result.beads_synced, 1);
    assert_eq!(result.beads_added, 1);
    assert_eq!(result.dependencies_updated, 0);
    assert_eq!(result.errors.len(), 1);
    assert_eq!(result.errors[0].operation, "dep_list");
    Ok(())
}

fn sample_issue(
    id: &str,
    title: &str,
    blocked_by: Vec<BeadId>,
    blocks: Vec<BeadId>,
) -> Result<BrIssueSummary, Box<dyn Error>> {
    let created_at: Timestamp = "2026-03-16T10:00:00Z".parse()?;
    let updated_at: Timestamp = "2026-03-16T11:00:00Z".parse()?;
    Ok(BrIssueSummary {
        id: BeadId::new(id),
        title: title.into(),
        description: Some(format!("description for {title}")),
        priority: BeadPriority::P0,
        issue_type: "task".into(),
        status: "open".into(),
        assignee: None,
        labels: vec!["area:test".into()],
        created_at,
        updated_at,
        blocked_by,
        blocks,
        raw_json: json!({"id": id}),
    })
}

#[test]
fn crate_surface_exposes_capability_shape() -> TestResult {
    let client = FakeBrClient {
        ready: Vec::new(),
        list_open: Vec::new(),
        dep_snapshots: BTreeMap::new(),
        dep_failures: BTreeMap::new(),
    };

    let capability = client.capability()?;
    let version = capability
        .version
        .ok_or_else(|| IoError::other("missing version"))?;
    assert_eq!(version.patch, Some(12));
    Ok(())
}
