//! Rebuild `index.sqlite` rows for a workspace from its on-disk canonical
//! bundles. Recovers from rsync/manual bundle moves and repairs index drift:
//! bundle directories are the source of truth, `allocator_state` is preserved
//! (only bumped upward).

use std::collections::BTreeSet;

use orbit_common::OrbitError;
use orbit_common::fs::io::with_exclusive_file_lock;
use orbit_types::task::is_valid_orb_task_id;

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
    let mut indexed = 0;
    let mut readable = Vec::new();
    let mut failures = Vec::new();
    for task_id in &candidates {
        let dir = registry.canonical_task_bundle_path(&workspace_id, task_id)?;
        let result = with_exclusive_file_lock(&bundle_lock_target(&dir), "task reindex", || {
            if store.recover_deletion(task_id)? {
                removed_stale += 1;
                return Ok(());
            }
            if !dir.try_exists()? {
                if registry.unregister_task_bundle(task_id, &workspace_id)? {
                    removed_stale += 1;
                }
                return Ok(());
            }
            recover_pending_bundle_at(&dir)?;
            registry.register_task_bundle(task_id, &workspace_id, &dir)?;
            readable.push(task_id);
            Ok::<(), OrbitError>(())
        });
        if let Err(error) = result {
            // Keep unresolved bytes AND any authoritative binding/index. A
            // healthy neighbor still gets repaired, but this run cannot succeed.
            failures.push(format!("{task_id}: {error}"));
        }
    }

    // Register the readable set before validating relations: imported tasks
    // may refer to another bundle later in directory order. Re-read under the
    // lock so an intervening update/deletion cannot publish a stale index.
    for task_id in readable {
        let dir = registry.canonical_task_bundle_path(&workspace_id, task_id)?;
        let result = with_exclusive_file_lock(&bundle_lock_target(&dir), "task reindex", || {
            if store.recover_deletion(task_id)? || !dir.try_exists()? {
                return Ok(());
            }
            let bundle = recover_pending_bundle_at(&dir)?;
            registry.replace_task_index(&workspace_id, &bundle.envelope)?;
            indexed += 1;
            Ok::<(), OrbitError>(())
        });
        if let Err(error) = result {
            failures.push(format!("{task_id}: {error}"));
        }
    }

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
