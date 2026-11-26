use orbit_core::OrbitRuntime;
use orbit_core::runtime::JobRuns;
use orbit_store::TaskV2Store;
use std::sync::Arc;

pub(super) fn jobs(runtime: &OrbitRuntime) -> Arc<JobRuns> {
    orbit_core::runtime::JobRuns::shared(
        runtime.sqlite_store().unwrap(),
        runtime.workspace_id().unwrap(),
    )
}
pub(super) fn plugins(runtime: &OrbitRuntime) -> orbit_store::Store {
    runtime.sqlite_store().unwrap()
}
pub(super) fn tasks(runtime: &OrbitRuntime) -> Arc<TaskV2Store> {
    use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};
    let registry = TaskRegistryStore::open(&task_registry_path(&runtime.global_root())).unwrap();
    orbit_store::compose::workspace_coordinated_backends(
        registry,
        runtime.workspace_id().unwrap(),
        runtime.sqlite_store().unwrap(),
    )
    .unwrap()
    .task
    .task
}
