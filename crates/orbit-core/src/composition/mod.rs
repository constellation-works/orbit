//! Composition root joining resolved configuration, the runtime kernel, and adapters.

mod application_port;
mod runtime;
mod services;

pub use orbit_store::compose::compiled_compatibility;

pub use self::runtime::{pin_executable_generation, pin_executable_generation_as};
pub use self::services::host_resource_monitor;
pub use crate::bootstrap::workspace_authority::{
    AuthorityAdoptionEvidence, WorkspaceAuthorityAdoption, adopt_workspace_layout_authority,
    layout_migration_fenced_authorities, verify_workspace_layout_authority,
};
pub use crate::runtime::{captured_env, install_captured_env};

/// Return the environment captured by Orbit's entry point, or capture it when
/// called from a process that did not install one.
pub fn composition_env() -> orbit_common::Env {
    crate::runtime::process_env()
}

/// Supply the captured process snapshot to composition callers.
pub fn process_env() -> orbit_common::Env {
    crate::runtime::process_env()
}
