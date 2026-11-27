//! Persistence refresh after a CLI provider run, at the engine boundary.

use orbit_common::OrbitError;

use crate::OrbitRuntime;

pub(super) fn refresh_persistence_after_cli_provider(
    runtime: &OrbitRuntime,
) -> Result<(), OrbitError> {
    runtime
        .sqlite_store()?
        .refresh_file_connections(&runtime.persistence().audit_db)
}
