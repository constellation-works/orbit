//! Versioned logical workspace catalog and machine-local checkout bindings.

mod legacy;
mod lookup;
mod mutations;
mod validation;

pub use lookup::{
    find_checkout, find_checkout_by_id, find_checkout_by_path, find_workspace,
    find_workspace_by_id, find_workspace_by_path, local_workspaces, resolve_logical_workspace,
};
pub use mutations::{
    WorkspaceSourceRemoteRebind, assign_checkout_role, rebind_workspace_source_remote,
    register_checkout, register_workspace, remove_workspace, set_path_override,
};
pub use validation::{parse_workspace_registry, validate_workspace_registry, validate_workspaces};

/// Machine identity facts used while validating a local registry file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceRegistryMachineContext {
    pub machine_id: Option<String>,
}
