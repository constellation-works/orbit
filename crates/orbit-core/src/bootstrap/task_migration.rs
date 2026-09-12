//! Runtime facades over the `orbit-store` task-migration engine.
//!
//! These keep the crate boundary intact: `orbit-cli` calls these methods /
//! functions rather than opening the `TaskRegistryStore` itself. The heavy
//! lifting (archive packing, transactional import, reindex) lives in
//! [`orbit_store::workflow::task`]; this layer only resolves the registry path,
//! the target workspace id, and the clock.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_store::maintenance::task_registry::{
    AllocatorSeedOutcome, BindWorkspaceParams, TaskRegistryStore, task_registry_path,
    task_workspaces_dir,
};
use orbit_store::workflow::task::{export_tasks, import_tasks, reindex_workspace};
use orbit_types::task::is_valid_orb_task_id;

use crate::OrbitRuntime;

// Re-export the engine's public types so `orbit-cli` (which depends on
// orbit-core, not orbit-store) can name them without crossing the crate
// boundary.
pub use orbit_store::maintenance::task_registry::DanglingRelationTarget;
pub use orbit_store::workflow::task::{
    ExportOutcome, ExportSelection, ImportAction, ImportConflictPolicy, ImportOutcome,
    ImportedTask, ReindexOutcome,
};

impl OrbitRuntime {
    fn open_task_registry(&self) -> Result<TaskRegistryStore, OrbitError> {
        TaskRegistryStore::open(&task_registry_path(&self.global_root()))
    }

    /// Resolve the workspace id a migration command targets: an explicit
    /// `--task-workspace <id>` (a task-registry workspace id like `orbit-8fb91e`) or,
    /// when absent, the current workspace.
    fn resolve_migration_workspace(
        &self,
        workspace_id: Option<&str>,
    ) -> Result<String, OrbitError> {
        match workspace_id {
            Some(id) => Ok(id.to_string()),
            None if self.global_root() == self.paths().orbit_dir => {
                match self.workspace_runtime_binding() {
                    Some(binding) => Ok(binding.logical_workspace_id.clone()),
                    None => self.workspace_id(),
                }
            }
            None => self.workspace_id(),
        }
    }

    /// Rebuild the selected checkout's task-registry binding when an explicit
    /// root recreated `tasks/index.sqlite` without restoring its rows.
    fn ensure_migration_workspace_binding(
        &self,
        registry: &TaskRegistryStore,
        workspace_id: &str,
    ) -> Result<(), OrbitError> {
        if registry.find_workspace_binding(workspace_id)?.is_some() {
            return Ok(());
        }
        let Some(binding) = self.workspace_runtime_binding() else {
            return Ok(());
        };
        if binding.logical_workspace_id != workspace_id {
            return Ok(());
        }

        let slug = binding
            .repo_root
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or("workspace")
            .to_string();
        registry.bind_workspace(BindWorkspaceParams {
            partition_id: Some(workspace_id.to_string()),
            slug,
            repo_root: binding.repo_root.clone(),
            workspace_path: binding.repo_root.clone(),
            orbit_dir: self.paths().orbit_dir.clone(),
            repo_fingerprint: None,
        })?;
        Ok(())
    }

    /// Export the selected tasks of a workspace to a tar.zst archive.
    pub fn export_tasks(
        &self,
        workspace_id: Option<&str>,
        selection: ExportSelection,
        out_path: &Path,
    ) -> Result<ExportOutcome, OrbitError> {
        let registry = self.open_task_registry()?;
        let workspace_id = self.resolve_migration_workspace(workspace_id)?;
        export_tasks(&registry, &workspace_id, selection, out_path, Utc::now())
    }

    /// Import tasks from a tar.zst archive into the local registry.
    pub fn import_tasks(
        &self,
        archive_path: &Path,
        target_workspace_id: Option<&str>,
        policy: ImportConflictPolicy,
    ) -> Result<ImportOutcome, OrbitError> {
        let registry = self.open_task_registry()?;
        import_tasks(&registry, archive_path, target_workspace_id, policy)
    }

    /// Rebuild `index.sqlite` rows for a workspace from its on-disk bundles.
    pub fn reindex_tasks(&self, workspace_id: Option<&str>) -> Result<ReindexOutcome, OrbitError> {
        let registry = self.open_task_registry()?;
        let workspace_id = self.resolve_migration_workspace(workspace_id)?;
        self.ensure_migration_workspace_binding(&registry, &workspace_id)?;
        reindex_workspace(&registry, &workspace_id)
    }

    /// Return the number of canonical task bundles that are present on disk
    /// but absent from the generated registry index.
    pub fn unindexed_task_bundle_count(&self) -> Result<usize, OrbitError> {
        let workspace_id = self.resolve_migration_workspace(None)?;
        let registry = self.open_task_registry()?;
        let indexed = registry
            .tasks_for_workspace(&workspace_id)?
            .into_iter()
            .map(|binding| binding.task_id)
            .collect::<BTreeSet<_>>();
        let partition = task_workspaces_dir(&self.global_root()).join(workspace_id);
        let entries = match fs::read_dir(partition) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(OrbitError::Io(error.to_string())),
        };
        let mut unindexed = 0;
        for entry in entries {
            let entry = entry.map_err(|error| OrbitError::Io(error.to_string()))?;
            let is_bundle = entry
                .file_type()
                .map_err(|error| OrbitError::Io(error.to_string()))?
                .is_dir();
            if is_bundle
                && entry
                    .file_name()
                    .to_str()
                    .is_some_and(|id| is_valid_orb_task_id(id) && !indexed.contains(id))
            {
                unindexed += 1;
            }
        }
        Ok(unindexed)
    }

    /// Audit task relation/dependency targets that no longer resolve to a
    /// registered task bundle — the grandfathered relations that make an index
    /// rebuild fail its validator (ORB-10305). Pass `Some(workspace_id)` to
    /// scope the sweep to one workspace, or `None` to audit the whole
    /// coordination registry.
    pub fn audit_dangling_relations(
        &self,
        workspace_id: Option<&str>,
    ) -> Result<Vec<DanglingRelationTarget>, OrbitError> {
        let registry = self.open_task_registry()?;
        registry.dangling_relation_targets(workspace_id)
    }
}

/// Seed the task-id allocator so the next allocated id is `start`. Used by
/// `orbit workspace init --task-id-start N` before a runtime exists. When a
/// host identity supplies a prefix, adopt it before advancing the counter; the
/// counter only moves forward, so a value below the current position is refused.
pub fn seed_task_id_start(
    global_root: &Path,
    task_prefix: Option<&str>,
    start: u32,
) -> Result<AllocatorSeedOutcome, OrbitError> {
    let registry = TaskRegistryStore::open(&task_registry_path(global_root))?;

    if let Some(task_prefix) = task_prefix {
        registry.set_task_prefix(task_prefix)?;
    }

    registry.seed_allocator_start(start)
}

/// Apply a configured `tasks.id_start` floor to the allocator. Unlike the
/// explicit CLI flag this never errors on an already-advanced counter — it only
/// raises the floor — so it is safe to call on every runtime build.
pub fn apply_configured_id_start(global_root: &Path, start: u32) -> Result<(), OrbitError> {
    let registry = TaskRegistryStore::open(&task_registry_path(global_root))?;
    registry.bump_allocator_to_at_least(start)
}
