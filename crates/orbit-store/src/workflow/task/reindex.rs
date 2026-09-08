//! Rebuild `index.sqlite` rows for a workspace from its on-disk canonical
//! bundles. Recovers from rsync/manual bundle moves and repairs index drift:
//! bundle directories are the source of truth, `allocator_state` is preserved
//! (only bumped upward), and the `.orbit/tasks/` projection is recreated.

use std::collections::BTreeSet;

use orbit_common::OrbitError;
use orbit_types::task::is_valid_orb_task_id;

use crate::driver::file::task_bundle::recover_pending_bundle_at;
use crate::driver::sqlite::task_registry::{
    ProjectionRebuildResult, TaskRegistryStore, parse_orb_task_number,
};

/// Result of [`reindex_workspace`].
#[derive(Debug, Clone)]
pub struct ReindexOutcome {
    /// Workspace that was reindexed.
    pub workspace_id: String,
    /// Number of on-disk bundles registered/indexed.
    pub indexed: usize,
    /// Number of stale registry bindings dropped (bundle no longer on disk).
    pub removed_stale: usize,
    /// Projection rebuild result.
    pub projection: ProjectionRebuildResult,
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
    let workspace_id = binding.workspace_id.clone();

    // Enumerate the bundles physically present on disk.
    let workspace_dir = registry.workspaces_dir().join(&workspace_id);
    let on_disk = on_disk_task_ids(&workspace_dir)?;

    // Drop registry bindings whose bundle directory is gone — bundle dirs are the
    // source of truth, and replace_workspace_task_indexes requires the registered
    // set to equal the envelope set.
    let mut removed_stale = 0usize;
    for existing in registry.tasks_for_workspace(&workspace_id)? {
        if !on_disk.contains(&existing.task_id)
            && registry.unregister_task_bundle(&existing.task_id, &workspace_id)?
        {
            removed_stale += 1;
        }
    }

    // Register every on-disk bundle (idempotent upsert) and collect envelopes.
    // An incomplete multi-file write is aborted or finalized from its pending record.
    let mut envelopes = Vec::with_capacity(on_disk.len());
    let mut max_number: Option<u32> = None;
    for task_id in &on_disk {
        let dir = registry.canonical_task_bundle_path(&workspace_id, task_id)?;
        let bundle = recover_pending_bundle_at(&dir)?;
        registry.register_task_bundle(task_id, &workspace_id, &dir)?;
        if let Some(number) = parse_orb_task_number(task_id) {
            max_number = Some(max_number.map_or(number, |current| current.max(number)));
        }
        envelopes.push(bundle.envelope);
    }

    registry.replace_workspace_task_indexes(&workspace_id, &envelopes)?;

    // Never let the allocator hand out an id that already exists on disk.
    if let Some(max) = max_number {
        registry.bump_allocator_to_at_least(max + 1)?;
    }

    let projection = if let Some(checkout) = registry.find_workspace_checkout(&workspace_id)? {
        crate::repository::checkout_projection::rebuild_projection(
            registry,
            &checkout.orbit_dir,
            &workspace_id,
        )?
    } else {
        ProjectionRebuildResult {
            projected: 0,
            repaired: 0,
            degraded_reason: None,
        }
    };

    Ok(ReindexOutcome {
        workspace_id,
        indexed: on_disk.len(),
        removed_stale,
        projection,
    })
}

/// Canonical task ids that have a bundle directory on disk under `workspace_dir`.
fn on_disk_task_ids(workspace_dir: &std::path::Path) -> Result<BTreeSet<String>, OrbitError> {
    let mut ids = BTreeSet::new();
    let entries = match std::fs::read_dir(workspace_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(ids),
        Err(err) => return Err(OrbitError::Io(err.to_string())),
    };
    for entry in entries {
        let entry = entry.map_err(|e| OrbitError::Io(e.to_string()))?;
        if !entry
            .file_type()
            .map_err(|e| OrbitError::Io(e.to_string()))?
            .is_dir()
        {
            continue;
        }
        if let Some(name) = entry.file_name().to_str()
            && is_valid_orb_task_id(name)
        {
            ids.insert(name.to_string());
        }
    }
    Ok(ids)
}
