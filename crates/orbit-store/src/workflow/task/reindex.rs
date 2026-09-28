//! Rebuild `index.sqlite` rows for a workspace from its on-disk canonical
//! bundles. Recovers from rsync/manual bundle moves and repairs index drift:
//! bundle directories are the source of truth, `allocator_state` is preserved
//! (only bumped upward).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::io::with_exclusive_file_lock;
use orbit_types::task::{TaskEnvelopeV2, is_valid_orb_task_id, task_id_prefix};

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
    let local_prefix = registry.local_task_prefix()?;
    let max_number = candidates
        .iter()
        .filter(|id| task_id_prefix(id) == Some(local_prefix.as_str()))
        .filter_map(|id| parse_orb_task_number(id))
        .max();
    for existing in registry.tasks_for_workspace(&workspace_id)? {
        candidates.insert(existing.task_id);
    }
    let store = TaskBundleStoreV2::new(registry.clone(), workspace_id.clone());
    let candidates = candidates
        .into_iter()
        .map(|task_id| {
            let dir = registry.canonical_task_bundle_path(&workspace_id, &task_id)?;
            Ok((task_id, dir))
        })
        .collect::<Result<Vec<_>, OrbitError>>()?;
    let mut removed_stale = 0;
    let mut snapshots: Vec<(String, PathBuf)> = Vec::new();
    let mut failures = Vec::new();
    // Deletion recovery, the existence check, the first settled read and the
    // binding stay under the bundle locks: dropping or adding a binding must
    // not race a creator or deleter of the same id. Every readable binding
    // lands before any index row, so a relation may name a task in a later
    // batch. The envelopes read here are *not* what gets indexed — a
    // concurrent update or delete can land after these locks are dropped.
    for batch in candidates.chunks(REINDEX_LOCK_BATCH) {
        let readable = with_candidate_locks(batch, || {
            let readable = inspect_batch(
                &store,
                registry,
                &workspace_id,
                batch,
                &mut removed_stale,
                &mut failures,
            )
            .into_iter()
            .map(|(task_id, dir, _)| (task_id, dir))
            .collect::<Vec<_>>();
            registry.register_task_bundles(&workspace_id, &readable)?;
            Ok(readable)
        })?;
        snapshots.extend(readable);
    }

    // Tests inject an update or delete in this window, matching a concurrent
    // writer that ran after the first read and before publication.
    #[cfg(test)]
    run_after_snapshot_hook();

    // Publish in bounded batches. Each batch takes its bundle locks in task-id
    // order and retains them from the final envelope read through the index
    // commit. Writers and deleters take a bundle lock before the registry, so
    // none can publish a newer state in between. Holding every lock at once
    // would keep one descriptor open per task. A fixed order avoids deadlocks
    // with other reindexes.
    let mut indexed = 0;
    let mut deferred: Vec<(String, PathBuf)> = Vec::new();
    for batch in snapshots.chunks(REINDEX_LOCK_BATCH) {
        indexed += with_candidate_locks(batch, || {
            let readable = inspect_batch(
                &store,
                registry,
                &workspace_id,
                batch,
                &mut removed_stale,
                &mut failures,
            );
            #[cfg(test)]
            run_before_reindex_publication_hook(batch);
            let envelopes = readable
                .iter()
                .map(|(_, _, envelope)| envelope.clone())
                .collect::<Vec<_>>();
            if registry
                .replace_task_indexes(&workspace_id, &envelopes)
                .is_ok()
            {
                return Ok(envelopes.len());
            }
            // Relation validation also walks rows of tasks outside this batch,
            // which may be stale rows a later batch replaces.
            deferred.extend(readable.into_iter().map(|(task_id, dir, _)| (task_id, dir)));
            Ok(0)
        })?;
    }
    // Publishing a later batch may unblock an earlier one, and that earlier
    // batch may in turn unblock another. Only a pass with no successful batch
    // is stalled; failed attempts before that are not unresolved bundles.
    while !deferred.is_empty() {
        let mut remaining = Vec::new();
        let mut published = 0;
        for batch in deferred.chunks(REINDEX_LOCK_BATCH) {
            published += with_candidate_locks(batch, || {
                let readable = inspect_batch(
                    &store,
                    registry,
                    &workspace_id,
                    batch,
                    &mut removed_stale,
                    &mut failures,
                );
                let envelopes = readable
                    .iter()
                    .map(|(_, _, envelope)| envelope.clone())
                    .collect::<Vec<_>>();
                if registry
                    .replace_task_indexes(&workspace_id, &envelopes)
                    .is_ok()
                {
                    return Ok(envelopes.len());
                }
                remaining.extend(readable.into_iter().map(|(task_id, dir, _)| (task_id, dir)));
                Ok(0)
            })?;
        }
        indexed += published;
        if published == 0 {
            // A genuinely invalid relation cannot become valid by retrying
            // the same rows. Repair any healthy members and retain errors.
            for batch in remaining.chunks(REINDEX_LOCK_BATCH) {
                indexed += with_candidate_locks(batch, || {
                    let envelopes = inspect_batch(
                        &store,
                        registry,
                        &workspace_id,
                        batch,
                        &mut removed_stale,
                        &mut failures,
                    )
                    .into_iter()
                    .map(|(_, _, envelope)| envelope)
                    .collect::<Vec<_>>();
                    Ok(index_healthy_set(
                        registry,
                        &workspace_id,
                        &envelopes,
                        &mut failures,
                    ))
                })?;
            }
            break;
        }
        deferred = remaining;
    }

    // Include unresolved IDs so allocator recovery cannot collide with data
    // retained for repair. Never replace the entire index with a partial set.
    if let Some(max) = max_number {
        registry.bump_allocator_past_task_number(max)?;
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

/// Most bundle locks one reindex holds at once. Each held lock keeps a file
/// descriptor open and a stack frame live, so this bounds both regardless of
/// workspace size while still amortizing registry commits across a batch.
pub(crate) const REINDEX_LOCK_BATCH: usize = 64;

/// Run `op` while holding the bundle lock of every candidate, acquired in
/// slice order. Callers pass at most [`REINDEX_LOCK_BATCH`] candidates.
fn with_candidate_locks<T, F>(candidates: &[(String, PathBuf)], op: F) -> Result<T, OrbitError>
where
    F: FnOnce() -> Result<T, OrbitError>,
{
    let Some((_, dir)) = candidates.first() else {
        return op();
    };
    with_exclusive_file_lock(&bundle_lock_target(dir), "task reindex", || {
        with_candidate_locks(&candidates[1..], op)
    })
}

/// Inspect every candidate of a locked batch, returning the settled envelope
/// of each one still readable. Unreadable candidates become failures: their
/// bytes and any authoritative binding/index are kept, and a healthy neighbor
/// still gets repaired, but the run cannot succeed.
fn inspect_batch(
    store: &TaskBundleStoreV2,
    registry: &TaskRegistryStore,
    workspace_id: &str,
    batch: &[(String, PathBuf)],
    removed_stale: &mut usize,
    failures: &mut Vec<String>,
) -> Vec<(String, PathBuf, TaskEnvelopeV2)> {
    let mut readable = Vec::new();
    for (task_id, dir) in batch {
        match inspect_candidate(store, registry, workspace_id, task_id, dir, removed_stale) {
            Ok(Some(envelope)) => readable.push((task_id.clone(), dir.clone(), envelope)),
            Ok(None) => {}
            Err(error) => failures.push(format!("{task_id}: {error}")),
        }
    }
    readable
}

/// Index a set in one commit, returning how many tasks landed.
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

/// A one-shot publication callback and the task id whose batch triggers it.
#[cfg(test)]
type PublicationHook = (String, Box<dyn FnOnce() + 'static>);

#[cfg(test)]
thread_local! {
    static AFTER_SNAPSHOT: std::cell::RefCell<Option<Box<dyn FnOnce() + 'static>>> =
        std::cell::RefCell::new(None);
    static BEFORE_PUBLICATION: std::cell::RefCell<Option<PublicationHook>> =
        std::cell::RefCell::new(None);
}

/// Install a one-shot callback that runs after the binding pass and before
/// the first publication batch re-reads its envelopes.
#[cfg(test)]
pub(crate) fn set_after_reindex_snapshot_hook(hook: impl FnOnce() + 'static) {
    AFTER_SNAPSHOT.with(|cell| *cell.borrow_mut() = Some(Box::new(hook)));
}

#[cfg(test)]
pub(crate) fn clear_after_reindex_snapshot_hook() {
    AFTER_SNAPSHOT.with(|cell| cell.borrow_mut().take());
}

/// Install a one-shot callback that runs after the final envelope reads of the
/// publication batch containing `task_id`, while that batch's locks are held
/// and before its index commit.
#[cfg(test)]
pub(crate) fn set_before_reindex_publication_hook(task_id: &str, hook: impl FnOnce() + 'static) {
    BEFORE_PUBLICATION
        .with(|cell| *cell.borrow_mut() = Some((task_id.to_string(), Box::new(hook))));
}

#[cfg(test)]
pub(crate) fn clear_before_reindex_publication_hook() {
    BEFORE_PUBLICATION.with(|cell| cell.borrow_mut().take());
}

#[cfg(test)]
fn run_before_reindex_publication_hook(batch: &[(String, PathBuf)]) {
    let hook = BEFORE_PUBLICATION.with(|cell| {
        let mut slot = cell.borrow_mut();
        let due = slot
            .as_ref()
            .is_some_and(|(target, _)| batch.iter().any(|(task_id, _)| task_id == target));
        if due { slot.take() } else { None }
    });
    if let Some((_, hook)) = hook {
        hook();
    }
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
