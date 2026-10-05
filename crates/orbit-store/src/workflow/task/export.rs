//! Export of a workspace's task bundles to a portable tar.zst archive.

use crate::driver::sqlite::task_registry::TaskRegistryStore;
use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::task::{TASK_ARTIFACT_SCHEMA_VERSION, validate_orb_task_id};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::archive;
use super::manifest::{MIGRATION_FORMAT_VERSION, TaskMigrationManifest};

/// Which tasks an export should include.
#[derive(Debug, Clone)]
pub enum ExportSelection {
    /// Every task registered to the workspace.
    All,
    /// An explicit set of task ids (each must be registered to the workspace).
    Ids(Vec<String>),
}

/// Result of [`export_tasks`].
#[derive(Debug, Clone)]
pub struct ExportOutcome {
    /// Path the archive was written to.
    pub archive_path: PathBuf,
    /// Source workspace id.
    pub workspace_id: String,
    /// Task ids written into the archive.
    pub task_ids: Vec<String>,
}

/// Export the selected tasks of `workspace_id` to a tar.zst archive at `out_path`.
pub fn export_tasks(
    registry: &TaskRegistryStore,
    workspace_id: &str,
    selection: ExportSelection,
    out_path: &Path,
    exported_at: DateTime<Utc>,
) -> Result<ExportOutcome, OrbitError> {
    let binding = registry
        .find_workspace_binding(workspace_id)?
        .ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "workspace '{workspace_id}' is not registered in the coordination registry"
            ))
        })?;
    let workspace_id = binding.partition_id.clone();

    let registered: BTreeSet<String> = registry
        .tasks_for_workspace(&workspace_id)?
        .into_iter()
        .map(|task| task.task_id)
        .collect();

    let task_ids: Vec<String> = match selection {
        ExportSelection::All => registered.iter().cloned().collect(),
        ExportSelection::Ids(ids) => {
            let mut resolved = Vec::new();
            let mut seen = BTreeSet::new();
            for raw in ids {
                let id = raw.trim().to_string();
                validate_orb_task_id(&id)?;
                if !registered.contains(&id) {
                    return Err(OrbitError::InvalidInput(format!(
                        "task '{id}' is not registered to workspace '{workspace_id}'"
                    )));
                }
                if seen.insert(id.clone()) {
                    resolved.push(id);
                }
            }
            resolved.sort();
            resolved
        }
    };

    if task_ids.is_empty() {
        return Err(OrbitError::InvalidInput(format!(
            "workspace '{workspace_id}' has no tasks to export"
        )));
    }

    let mut bundle_dirs = Vec::with_capacity(task_ids.len());
    for id in &task_ids {
        let dir = registry.canonical_task_bundle_path(&workspace_id, id)?;
        if !dir.is_dir() {
            return Err(OrbitError::Store(format!(
                "canonical bundle for '{id}' is missing at {}",
                dir.display()
            )));
        }
        bundle_dirs.push((id.clone(), dir));
    }

    let manifest = TaskMigrationManifest {
        format_version: MIGRATION_FORMAT_VERSION,
        task_schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
        source_workspace_id: workspace_id.clone(),
        source_workspace_slug: binding.slug.clone(),
        task_ids: task_ids.clone(),
        exported_at,
    };
    let manifest_json = serde_json::to_vec_pretty(&manifest)
        .map_err(|e| OrbitError::Store(format!("failed to encode manifest: {e}")))?;

    archive::write_archive(out_path, &manifest_json, &bundle_dirs)?;

    Ok(ExportOutcome {
        archive_path: out_path.to_path_buf(),
        workspace_id,
        task_ids,
    })
}
