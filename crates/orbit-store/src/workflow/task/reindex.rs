//! Rebuild `index.sqlite` rows for a workspace from its on-disk canonical
//! bundles. Recovers from rsync/manual bundle moves and repairs index drift:
//! bundle directories are the source of truth, `allocator_state` is preserved
//! (only bumped upward).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::io::with_exclusive_file_lock;
use orbit_types::task::{TaskEnvelopeV2, is_valid_orb_task_id};

use crate::driver::file::task_bundle::{
    bundle_lock_target, is_unpublished_stub, reap_unpublished_stub, recover_pending_bundle_at,
};
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
    let mut snapshots: Vec<(String, PathBuf)> = Vec::new();
    let mut failures = Vec::new();
    for task_id in &candidates {
        let dir = registry.canonical_task_bundle_path(&workspace_id, task_id)?;
        // Deletion recovery, the existence check and the first settled read
        // stay under the bundle lock: dropping a binding whose directory
        // vanished must not race a creator publishing the same id. The
        // envelope collected here is *not* what gets indexed — a concurrent
        // update or delete can land after this lock is dropped.
        match inspect_candidate(
            &store,
            registry,
            &workspace_id,
            task_id,
            &dir,
            &mut removed_stale,
        ) {
            Ok(Some(_)) => snapshots.push((task_id.clone(), dir)),
            Ok(None) => {}
            // Keep unresolved bytes AND any authoritative binding/index. A
            // healthy neighbor still gets repaired, but this run cannot succeed.
            Err(error) => failures.push(format!("{task_id}: {error}")),
        }
    }

    // Tests inject an update or delete in this window, matching a concurrent
    // writer that ran after the first read and before the batch.
    #[cfg(test)]
    run_after_snapshot_hook();

    // Re-check under each bundle lock immediately before the batch. Include
    // only the envelope just read from disk; drop tasks whose directory
    // vanished or whose deletion published. A changed `updated_at` is the
    // current envelope, never the first-pass snapshot.
    let mut readable: Vec<(String, PathBuf)> = Vec::new();
    let mut envelopes: Vec<TaskEnvelopeV2> = Vec::new();
    for (task_id, dir) in snapshots {
        match inspect_candidate(
            &store,
            registry,
            &workspace_id,
            &task_id,
            &dir,
            &mut removed_stale,
        ) {
            Ok(Some(envelope)) => {
                readable.push((task_id, dir));
                envelopes.push(envelope);
            }
            Ok(None) => {}
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

/// Recover a published deletion or vanished directory, then read a settled
/// envelope. `None` means the task must not join the registry batch.
fn inspect_candidate(
    store: &TaskBundleStoreV2,
    registry: &TaskRegistryStore,
    workspace_id: &str,
    task_id: &str,
    dir: &Path,
    removed_stale: &mut usize,
) -> Result<Option<TaskEnvelopeV2>, OrbitError> {
    with_exclusive_file_lock(&bundle_lock_target(dir), "task reindex", || {
        if store.recover_deletion(task_id)? {
            *removed_stale += 1;
            return Ok(None);
        }
        if !dir.try_exists()? {
            if registry.unregister_task_bundle(task_id, workspace_id)? {
                *removed_stale += 1;
            }
            return Ok(None);
        }
        // Aborted creates leave a valid ORB-* directory with no task.yaml
        // (empty, or only `.task.yaml.lock`). That is garbage, not an
        // unresolved bundle: reap it so a healthy neighbor can still be
        // indexed. A directory missing task.yaml but holding any other
        // entry is unresolved data and must fail closed.
        if is_unpublished_stub(dir) {
            if let Err(error) = reap_unpublished_stub(dir) {
                orbit_common::tracing::warn!(
                    target: "orbit.store.task_reindex",
                    bundle_dir = %dir.display(),
                    error = %error,
                    "failed to reap unpublished task-bundle stub; skipping",
                );
            }
            if registry.unregister_task_bundle(task_id, workspace_id)? {
                *removed_stale += 1;
            }
            return Ok(None);
        }
        Ok(Some(recover_pending_bundle_at(dir)?.envelope))
    })
}

#[cfg(test)]
thread_local! {
    static AFTER_SNAPSHOT: std::cell::RefCell<Option<Box<dyn FnOnce() + 'static>>> =
        std::cell::RefCell::new(None);
}

/// Install a one-shot callback that runs after the first envelope pass and
/// immediately before the freshness re-check that builds the registry batch.
#[cfg(test)]
pub(crate) fn set_after_reindex_snapshot_hook(hook: impl FnOnce() + 'static) {
    AFTER_SNAPSHOT.with(|cell| *cell.borrow_mut() = Some(Box::new(hook)));
}

#[cfg(test)]
pub(crate) fn clear_after_reindex_snapshot_hook() {
    AFTER_SNAPSHOT.with(|cell| cell.borrow_mut().take());
}

#[cfg(test)]
fn run_after_snapshot_hook() {
    if let Some(hook) = AFTER_SNAPSHOT.with(|cell| cell.borrow_mut().take()) {
        hook();
    }
}

/// Candidate IDs include tombstones and malformed non-directory entries, so
/// reindex reports unresolved data rather than treating it as an absent bundle.
fn on_disk_task_ids(workspace_dir: &Path) -> Result<BTreeSet<String>, OrbitError> {
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
