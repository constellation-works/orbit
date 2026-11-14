//! Open-time task-id allocator seeding.
//!
//! The request-time export, import and reindex use cases live in
//! [`crate::application::task_bundle::migration`]; only the seeding that
//! runs before or at runtime open stays here.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_store::maintenance::task_registry::{
    AllocatorSeedOutcome, TaskRegistryStore, task_registry_path,
};

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
