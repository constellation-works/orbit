//! Runtime-owned access to the workspace-partitioned friction repository.

use orbit_common::OrbitError;
use orbit_store::compose::workspace_friction_store;
use orbit_store::contracts::FrictionStoreBackend;
use std::sync::Arc;

use crate::OrbitRuntime;

/// Friction repository scoped by this runtime's workspace identity.
///
/// Uses the runtime-owned host store rather than opening the audit database
/// again on every call.
pub(crate) fn store_for(
    runtime: &OrbitRuntime,
) -> Result<Arc<dyn FrictionStoreBackend>, OrbitError> {
    workspace_friction_store(
        runtime.sqlite_store()?,
        runtime.workspace_id()?,
        runtime.data_root().join("frictions"),
    )
}
