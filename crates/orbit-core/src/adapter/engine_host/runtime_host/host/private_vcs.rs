//! Routing for the engine's private VCS operations.

use orbit_common::OrbitError;
use serde_json::Value;

use super::EngineHost;

/// Run one private VCS operation. The engine's PR merge takes the audited
/// branch; every other operation runs the engine's private driver directly.
pub(super) fn run_private_vcs_operation(
    host: &EngineHost<'_>,
    operation: &str,
    input: Value,
) -> Result<Value, OrbitError> {
    let runtime = host.runtime;
    // [ORB-26389] Every merge the engine requests leaves an audit row.
    if operation == orbit_engine::PRIVATE_VCS_PR_MERGE {
        return crate::application::task::audited_private_vcs_merge(runtime, &input);
    }
    orbit_engine::run_private_vcs_operation(runtime.env(), operation, &input)
}
