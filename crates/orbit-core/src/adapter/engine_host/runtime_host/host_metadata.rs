//! Credential policy, workspace selector and persistence refresh after a CLI
//! provider run, at the engine boundary.

use orbit_common::OrbitError;

use crate::OrbitRuntime;

pub(super) fn requires_claude_worker_credential() -> bool {
    cfg!(target_os = "macos")
}

pub(super) fn orbit_workspace_selector(runtime: &OrbitRuntime) -> Option<String> {
    runtime
        .workspace_runtime_binding()
        .map(|binding| binding.logical_workspace_id.trim())
        .filter(|selector| !selector.is_empty())
        .map(ToOwned::to_owned)
}

pub(super) fn refresh_persistence_after_cli_provider(
    runtime: &OrbitRuntime,
) -> Result<(), OrbitError> {
    runtime
        .sqlite_store()?
        .refresh_file_connections(&runtime.context.persistence().audit_db)
}
