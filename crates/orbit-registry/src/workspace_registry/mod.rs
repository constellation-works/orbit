//! Logical workspace catalog and machine-local persistence.

mod catalog;
mod io;
mod publication;

pub use catalog::{
    WorkspaceRegistryHostContext, WorkspaceSourceRemoteRebind, assign_checkout_role, find_checkout,
    find_checkout_by_path, find_workspace, find_workspace_by_id, find_workspace_by_path,
    local_workspaces, parse_workspace_registry, rebind_workspace_source_remote, register_checkout,
    register_workspace, remove_workspace, rename_local_owner_host_id, resolve_logical_workspace,
    set_path_override, validate_workspace_registry, validate_workspaces,
};
pub use io::{
    global_orbit_dir, load_registry, load_registry_from, registry_path, registry_path_for,
    save_registry, save_registry_to, with_registry_lock,
};
pub use publication::{
    bind_publication, find_publication_binding, rebind_publication, record_publication_success,
    unbind_publication,
};

#[cfg(test)]
pub(crate) use io::load_registry_from_with_writer;
