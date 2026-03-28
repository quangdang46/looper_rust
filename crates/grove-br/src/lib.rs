mod client;
mod schema;

use anyhow::Result;
use grove_types::{BeadId, GroveBeadStatus};
use std::collections::{HashMap, HashSet};

pub use client::{BrClient, BrError, CliBrClient};
pub use schema::{
    BrCapability, BrComment, BrDependencyRow, BrDependencySnapshot, BrIssueDetail, BrIssueSummary,
    BrVersion, ShowParseError,
};

pub const CRATE_PURPOSE: &str = "Integration boundary for beads_rust (br).";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpsertOutcome {
    Added,
    Updated,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedBeadState {
    pub bead_id: BeadId,
    pub grove_status: Option<GroveBeadStatus>,
}

pub trait BeadCacheStore {
    fn upsert_bead_cache(&mut self, bead: &BrIssueSummary) -> Result<UpsertOutcome>;

    fn replace_dependency_snapshot(
        &mut self,
        bead_id: &BeadId,
        blocked_by: &[BeadId],
        blocks: &[BeadId],
    ) -> Result<()>;

    fn list_cached_beads(&self) -> Result<Vec<CachedBeadState>>;

    fn set_grove_status(&mut self, bead_id: &BeadId, status: GroveBeadStatus) -> Result<()>;

    fn remove_bead_cache(&mut self, bead_id: &BeadId) -> Result<()>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncError {
    pub bead_id: Option<BeadId>,
    pub operation: String,
    pub error: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncResult {
    pub beads_synced: usize,
    pub beads_added: usize,
    pub beads_updated: usize,
    pub beads_removed: usize,
    pub dependencies_updated: usize,
    pub errors: Vec<SyncError>,
}

pub fn sync_bead_cache<C: BrClient, S: BeadCacheStore>(
    br: &C,
    store: &mut S,
) -> Result<SyncResult> {
    let open_beads = br.list_open()?;
    let ready_beads = br.ready()?;
    let ready_ids: HashSet<BeadId> = ready_beads.into_iter().map(|bead| bead.id).collect();

    let cached = store.list_cached_beads()?;
    let cached_statuses: HashMap<BeadId, Option<GroveBeadStatus>> = cached
        .iter()
        .map(|entry| (entry.bead_id.clone(), entry.grove_status))
        .collect();
    let cached_ids: HashSet<BeadId> = cached.into_iter().map(|entry| entry.bead_id).collect();
    let remote_ids: HashSet<BeadId> = open_beads.iter().map(|bead| bead.id.clone()).collect();

    let mut result = SyncResult::default();

    for bead in &open_beads {
        match store.upsert_bead_cache(bead) {
            Ok(UpsertOutcome::Added) => result.beads_added += 1,
            Ok(UpsertOutcome::Updated) => result.beads_updated += 1,
            Err(error) => {
                result.errors.push(SyncError {
                    bead_id: Some(bead.id.clone()),
                    operation: "upsert_bead_cache".into(),
                    error: error.to_string(),
                });
                continue;
            }
        }

        let dependency_snapshot = if bead.blocked_by.is_empty() && bead.blocks.is_empty() {
            match br.dep_list(&bead.id) {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    result.errors.push(SyncError {
                        bead_id: Some(bead.id.clone()),
                        operation: "dep_list".into(),
                        error: error.to_string(),
                    });
                    result.beads_synced += 1;
                    continue;
                }
            }
        } else {
            bead.dependency_snapshot()
        };

        if let Err(error) = store.replace_dependency_snapshot(
            &bead.id,
            &dependency_snapshot.blocked_by,
            &dependency_snapshot.blocks,
        ) {
            result.errors.push(SyncError {
                bead_id: Some(bead.id.clone()),
                operation: "replace_dependency_snapshot".into(),
                error: error.to_string(),
            });
        } else {
            result.dependencies_updated += 1;
        }

        if ready_ids.contains(&bead.id)
            && matches!(
                cached_statuses.get(&bead.id).copied().flatten(),
                None | Some(GroveBeadStatus::Idle)
            )
            && let Err(error) = store.set_grove_status(&bead.id, GroveBeadStatus::Ready)
        {
            result.errors.push(SyncError {
                bead_id: Some(bead.id.clone()),
                operation: "set_grove_status".into(),
                error: error.to_string(),
            });
        }

        result.beads_synced += 1;
    }

    for bead_id in cached_ids.difference(&remote_ids) {
        if matches!(
            cached_statuses.get(bead_id).copied().flatten(),
            Some(GroveBeadStatus::Running | GroveBeadStatus::Checkpointed)
        ) {
            continue;
        }

        match store.remove_bead_cache(bead_id) {
            Ok(()) => result.beads_removed += 1,
            Err(error) => result.errors.push(SyncError {
                bead_id: Some((*bead_id).clone()),
                operation: "remove_bead_cache".into(),
                error: error.to_string(),
            }),
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests;
