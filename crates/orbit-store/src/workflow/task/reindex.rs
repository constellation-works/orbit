//! Rebuild `index.sqlite` rows for a workspace from its on-disk canonical
//! bundles. Recovers from rsync/manual bundle moves and repairs index drift:
//! bundle directories are the source of truth, `allocator_state` is preserved
//! (only bumped upward).

use std::collections::BTreeSet;
use std::path::PathBuf;

use orbit_common::OrbitError;
use orbit_common::fs::io::with_exclusive_file_lock;
use orbit_types::task::{TaskEnvelopeV2, is_valid_orb_task_id};

use crate::driver::file::task_bundle::{bundle_lock_target, recover_pending_bundle_at};
use crate::driver::sqlite::task_registry::{TaskRegistryStore, parse_orb_task_number};
use crate::repository::task::v2_bundle::TaskBundleStoreV2;

/// Result of [`reindex_workspace`].
#[derive(Debug, Clone)]
pub struct ReindexOutcome {
    /// Workspace that was reindexed.
    pub workspace_id: String,
    /// Number of on-disk bundles registered/indexed.
    pub indexed: usize,
    /// Number of stale registry bindings dropped (bundle no longer on disk).
    pub removed_stale: usize,
}

/// Rebuild the registry index rows for `workspace_id` from its canonical bundle
/// directories.
pub fn reindex_workspace(
    registry: &TaskRegistryStore,
    workspace_id: &str,
) -> Result<ReindexOutcome, OrbitError> {
    let binding = registry
        .find_workspace_binding(workspace_id)?
        .ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "workspace '{workspace_id}' is not registered in the coordination registry"
            ))
        })?;
    let workspace_id = binding.partition_id.clone();

    let workspace_dir = registry.workspaces_dir().join(&workspace_id);
    let mut candidates = on_disk_task_ids(&workspace_dir)?;
    let max_number = candidates
        .iter()
        .filter_map(|id| parse_orb_task_number(id))
        .max();
    for existing in registry.tasks_for_workspace(&workspace_id)? {
        candidates.insert(existing.task_id);
    }
    let store = TaskBundleStoreV2::new(registry.clone(), workspace_id.clone());
    let mut removed_stale = 0;
    let mut readable: Vec<(String, PathBuf)> = Vec::new();
    let mut envelopes: Vec<TaskEnvelopeV2> = Vec::new();
    let mut failures = Vec::new();
    for task_id in &candidates {
        let dir = registry.canonical_task_bundle_path(&workspace_id, task_id)?;
        // Deletion recovery, the existence check and the authoritative read all
        // stay under the bundle lock: dropping a binding whose directory
        // vanished must not race a creator publishing the same id, and the
        // envelope this run indexes must be a settled one. Only the registry
        // writes for the healthy set are lifted out and batched below.
        let result = with_exclusive_file_lock(&bundle_lock_target(&dir), "task reindex", || {
            if store.recover_deletion(task_id)? {
                removed_stale += 1;
                return Ok(None);
            }
            if !dir.try_exists()? {
                if registry.unregister_task_bundle(task_id, &workspace_id)? {
                    removed_stale += 1;
                }
                return Ok(None);
            }
            Ok::<Option<TaskEnvelopeV2>, OrbitError>(Some(
                recover_pending_bundle_at(&dir)?.envelope,
            ))
        });
        match result {
            Ok(Some(envelope)) => {
                readable.push((task_id.clone(), dir));
                envelopes.push(envelope);
            }
            Ok(None) => {}
            // Keep unresolved bytes AND any authoritative binding/index. A
            // healthy neighbor still gets repaired, but this run cannot succeed.
            Err(error) => failures.push(format!("{task_id}: {error}")),
        }
    }

    // One commit for every healthy binding, then one for the whole index batch,
    // instead of two per task. Registering the readable set before the index
    // pass is what lets a bundle refer to another that sorts after it; the
    // index batch then validates those relations as one set.
    registry.register_task_bundles(&workspace_id, &readable)?;
    let indexed = index_healthy_set(registry, &workspace_id, &envelopes, &mut failures);

    // Include unresolved IDs so allocator recovery cannot collide with data
    // retained for repair. Never replace the entire index with a partial set.
    if let Some(max) = max_number {
        registry.bump_allocator_to_at_least(max.saturating_add(1))?;
    }

    if !failures.is_empty() {
        return Err(OrbitError::Store(format!(
            "reindex incomplete: indexed {indexed} healthy tasks; unresolved bundles retained: {}",
            failures.join("; ")
        )));
    }

    Ok(ReindexOutcome {
        workspace_id,
        indexed,
        removed_stale,
    })
}

/// Index the readable set in one commit, returning how many tasks landed.
///
/// A batch that the registry refuses as a whole — a relation the set cannot
/// satisfy together, say — is retried one envelope at a time, because repairing
/// every task it still can is the entire point of reindex. The set-wide
/// rejection is recorded either way, so a run that needed the retry reports
/// itself incomplete rather than quietly downgrading validation.
fn index_healthy_set(
    registry: &TaskRegistryStore,
    workspace_id: &str,
    envelopes: &[TaskEnvelopeV2],
    failures: &mut Vec<String>,
) -> usize {
    let Err(batch_error) = registry.replace_task_indexes(workspace_id, envelopes) else {
        return envelopes.len();
    };
    failures.push(format!("task index batch: {batch_error}"));
    let mut indexed = 0;
    for envelope in envelopes {
        match registry.replace_task_index(workspace_id, envelope) {
            Ok(()) => indexed += 1,
            Err(error) => failures.push(format!("{}: {error}", envelope.id)),
        }
    }
    indexed
}

/// Candidate IDs include tombstones and malformed non-directory entries, so
/// reindex reports unresolved data rather than treating it as an absent bundle.
fn on_disk_task_ids(workspace_dir: &std::path::Path) -> Result<BTreeSet<String>, OrbitError> {
    let mut ids = BTreeSet::new();
    let entries = match std::fs::read_dir(workspace_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(ids),
        Err(err) => return Err(OrbitError::Io(err.to_string())),
    };
    for entry in entries {
        let entry = entry.map_err(|e| OrbitError::Io(e.to_string()))?;
        if let Some(name) = entry.file_name().to_str() {
            let id = name.strip_suffix(".deleted").unwrap_or(name);
            if is_valid_orb_task_id(id) {
                ids.insert(id.to_string());
            }
        }
    }
    Ok(ids)
}
