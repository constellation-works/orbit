//! Runtime facades over the `orbit-store` task-migration engine.
//!
//! These keep the crate boundary intact: `orbit-cli` calls these methods /
//! functions rather than opening the `TaskRegistryStore` itself. The heavy
//! lifting (archive packing, transactional import, reindex) lives in
//! [`orbit_store::workflow::task`]; this layer only resolves the registry path,
//! the target workspace id, and the clock.

use std::path::Path;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_store::maintenance::task_registry::{
    AllocatorSeedOutcome, TaskRegistryStore, task_registry_path,
};
use orbit_store::workflow::task::{export_tasks, import_tasks, reindex_workspace};

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
    /// `--workspace <id>` (a task-registry workspace id like `orbit-8fb91e`) or,
    /// when absent, the current workspace.
    fn resolve_migration_workspace(
        &self,
        workspace_id: Option<&str>,
    ) -> Result<String, OrbitError> {
        match workspace_id {
            Some(id) => Ok(id.to_string()),
            None => self.workspace_id(),
        }
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
        reindex_workspace(&registry, &workspace_id)
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
