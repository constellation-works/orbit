//! Logical workspace catalog and machine-local persistence.

mod catalog;
mod io;
mod publication;

pub use catalog::{
    WorkspaceRegistryHostContext, WorkspaceSourceRemoteRebind, assign_checkout_role, find_checkout,
    find_checkout_by_id, find_checkout_by_path, find_workspace, find_workspace_by_id,
    find_workspace_by_path, local_workspaces, parse_workspace_registry,
    rebind_workspace_source_remote, register_checkout, register_workspace, remove_workspace,
    rename_local_owner_host_id, resolve_logical_workspace, set_path_override,
    validate_workspace_registry, validate_workspaces,
};
pub use io::{
    ReadOnlyRegistryLoad, global_orbit_dir, load_registry, load_registry_from,
    load_registry_from_read_only, registry_path, registry_path_for, save_registry,
    save_registry_to, with_registry_lock,
};
pub use publication::{
    bind_publication, bind_publication_by_id, find_publication_binding,
    find_publication_binding_by_id, rebind_publication, rebind_publication_by_id,
    record_publication_success, record_publication_success_by_id, unbind_publication,
    unbind_publication_by_id,
};

#[cfg(test)]
pub(crate) use io::load_registry_from_with_writer;
