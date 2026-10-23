use orbit_core::OrbitRuntime;
use orbit_store::contracts::{JobRunStoreBackend, PluginStoreBackend, TaskStoreBackend};
use std::sync::Arc;

pub(super) fn jobs(runtime: &OrbitRuntime) -> Arc<dyn JobRunStoreBackend> {
    orbit_store::compose::workspace_job_run_store(
        runtime.sqlite_store().unwrap(),
        runtime.workspace_id().unwrap(),
    )
}
pub(super) fn plugins(runtime: &OrbitRuntime) -> Arc<dyn PluginStoreBackend> {
    orbit_store::compose::plugin_store_sqlite(runtime.sqlite_store().unwrap())
}
pub(super) fn tasks(runtime: &OrbitRuntime) -> Arc<dyn TaskStoreBackend> {
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
