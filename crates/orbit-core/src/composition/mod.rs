//! Composition root joining resolved configuration, the runtime kernel, and adapters.

mod application_port;
mod core;
mod runtime;
mod services;
mod store;

pub use orbit_store::compose::compiled_compatibility;

pub(crate) use self::core::Entry;
pub use self::core::{Core, core_for_command, core_for_service};
pub use self::runtime::{pin_executable_generation, pin_executable_generation_as};
pub use self::services::host_resource_monitor;
pub use self::store::store_opener;
pub(crate) use self::store::{open_store, open_store_read_only};
pub use crate::bootstrap::workspace_authority::{
    AuthorityAdoptionEvidence, WorkspaceAuthorityAdoption, adopt_workspace_layout_authority,
    layout_migration_fenced_authorities, verify_workspace_layout_authority,
};
pub use crate::runtime::{captured_env, install_captured_env};

/// The environment the composition root hands down: the one `main` captured,
/// or a fresh capture in a process that never ran `main`, such as a test
/// binary. Only composition reads the ambient process environment; core
/// application and adapter code receives this value from its caller or from
/// the runtime it was composed with.
pub(crate) fn process_env() -> orbit_common::Env {
    crate::runtime::process_env()
}

/// Return the environment captured by Orbit's entry point, or capture it when
/// called from a process that did not install one.
pub fn composition_env() -> orbit_common::Env {
    process_env()
}
