//! Task publication and the bundle-index count, which the adapter reaches
//! through a port bootstrap implements [ORB-22005]. Bootstrap keeps the
//! publication and migration code; the deterministic publication action and the
//! task list's empty-result hint call it here without naming bootstrap.

use orbit_common::OrbitError;
use serde_json::Value;

/// Bootstrap's task bundle operations for the adapter. Bootstrap implements it
/// on `OrbitRuntime` by delegating to its inherent methods.
pub(crate) trait TaskBundleMaintenancePort {
    /// Publish this workspace's tasks, as `run_task_publication` does, with an
    /// explicit successful skip when the workspace is unbound.
    fn run_workspace_task_publication(&self) -> Result<Value, OrbitError>;

    /// How many canonical task bundles are on disk but absent from the
    /// generated registry index, as `unindexed_task_bundle_count` counts them.
    fn count_unindexed_task_bundles(&self) -> Result<usize, OrbitError>;
}
