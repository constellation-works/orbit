//! Owner-local task-publication repository bindings.

use std::collections::HashSet;

use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::identity::validate_machine_id;
use orbit_types::workspace::{
    Workspace, WorkspaceCheckoutRole, WorkspaceError, WorkspacePublicationBinding,
    WorkspaceRegistry, canonicalize_publication_branch, git_remotes_equivalent, redact_git_remote,
    validate_publication_remote,
};

use super::{WorkspaceRegistryHostContext, find_checkout, find_workspace};

/// Locate the publication binding for a workspace id or name.
pub fn find_publication_binding<'a>(
    registry: &'a WorkspaceRegistry,
    id_or_name: &str,
) -> Result<Option<&'a WorkspacePublicationBinding>, OrbitError> {
    let Some(workspace) = find_workspace(registry, id_or_name)? else {
        return Ok(None);
    };
    Ok(registry
        .publication_bindings
        .iter()
        .find(|binding| binding.workspace_id == workspace.id))
}

/// Create a publication binding. Fails when one already exists; use
/// [`rebind_publication`] to replace it.
pub fn bind_publication(
    registry: &mut WorkspaceRegistry,
    id_or_name: &str,
    publication_remote: &str,
    publication_branch: &str,
    publication_id: &str,
    local_machine_id: Option<&str>,
) -> Result<WorkspacePublicationBinding, OrbitError> {
    let workspace_id = bindable_workspace(registry, id_or_name, local_machine_id)?.id;
    if registry
        .publication_bindings
        .iter()
        .any(|binding| binding.workspace_id == workspace_id)
    {
        return Err(publication_error(format!(
            "workspace '{workspace_id}' already has a publication binding; use rebind to replace it"
        )));
    }
    let binding = build_binding(
        registry,
        &workspace_id,
        publication_remote,
        publication_branch,
        publication_id,
    )?;
    registry.publication_bindings.push(binding.clone());
    Ok(binding)
}

/// Replace an existing publication binding. Last-success state is cleared
/// because the lineage or remote is changing.
pub fn rebind_publication(
    registry: &mut WorkspaceRegistry,
    id_or_name: &str,
    publication_remote: &str,
    publication_branch: &str,
    publication_id: &str,
    local_machine_id: Option<&str>,
) -> Result<WorkspacePublicationBinding, OrbitError> {
    let workspace_id = bindable_workspace(registry, id_or_name, local_machine_id)?.id;
    let index = registry
        .publication_bindings
        .iter()
        .position(|binding| binding.workspace_id == workspace_id)
        .ok_or_else(|| {
            publication_error(format!(
                "workspace '{workspace_id}' has no publication binding"
            ))
        })?;
    let binding = build_binding(
        registry,
        &workspace_id,
        publication_remote,
        publication_branch,
        publication_id,
    )?;
    registry.publication_bindings[index] = binding.clone();
    Ok(binding)
}

/// Remove the publication binding for a workspace.
pub fn unbind_publication(
    registry: &mut WorkspaceRegistry,
    id_or_name: &str,
    local_machine_id: Option<&str>,
) -> Result<WorkspacePublicationBinding, OrbitError> {
    let workspace_id = bindable_workspace(registry, id_or_name, local_machine_id)?.id;
    let index = registry
        .publication_bindings
        .iter()
        .position(|binding| binding.workspace_id == workspace_id)
        .ok_or_else(|| {
            publication_error(format!(
                "workspace '{workspace_id}' has no publication binding"
            ))
        })?;
    Ok(registry.publication_bindings.remove(index))
}

/// Record a successful publication generation and commit without rebinding.
pub fn record_publication_success(
    registry: &mut WorkspaceRegistry,
    id_or_name: &str,
    generation: u64,
    commit: &str,
    local_machine_id: Option<&str>,
) -> Result<WorkspacePublicationBinding, OrbitError> {
    let workspace_id = bindable_workspace(registry, id_or_name, local_machine_id)?.id;
    let binding = registry
        .publication_bindings
        .iter_mut()
        .find(|binding| binding.workspace_id == workspace_id)
        .ok_or_else(|| {
            publication_error(format!(
                "workspace '{workspace_id}' has no publication binding"
            ))
        })?;
    if let Some(previous) = binding.last_success_generation {
        if generation < previous {
            return Err(publication_error(format!(
                "workspace '{workspace_id}' publication generation {generation} is behind last success {previous}"
            )));
        }
        if generation == previous {
            if binding.last_success_commit.as_deref() == Some(commit) {
                return Ok(binding.clone());
            }
            return Err(publication_error(format!(
                "workspace '{workspace_id}' publication generation {generation} already recorded a different commit"
            )));
        }
    }
    let next = WorkspacePublicationBinding {
        workspace_id: binding.workspace_id.clone(),
        source_repository_fingerprint: binding.source_repository_fingerprint.clone(),
        publication_remote: binding.publication_remote.clone(),
        publication_branch: binding.publication_branch.clone(),
        publication_id: binding.publication_id.clone(),
        authority_machine_id: binding.authority_machine_id.clone(),
        last_success_generation: Some(generation),
        last_success_commit: Some(commit.to_ascii_lowercase()),
    }
    .validated()
    .map_err(workspace_error)?;
    *binding = next.clone();
    Ok(next)
}

pub(crate) fn validate_publication_bindings(
    registry: &WorkspaceRegistry,
    context: &WorkspaceRegistryHostContext,
) -> Result<(), OrbitError> {
    let mut seen_workspace_ids = HashSet::new();
    let mut seen_publication_ids = HashSet::new();
    for binding in &registry.publication_bindings {
        binding.validate().map_err(|error| {
            invalid_registry(format!(
                "publication binding for workspace '{}': {error}",
                binding.workspace_id
            ))
        })?;
        if !seen_workspace_ids.insert(binding.workspace_id.clone()) {
            return Err(invalid_registry(format!(
                "workspace '{}' has more than one publication binding",
                binding.workspace_id
            )));
        }
        if !seen_publication_ids.insert(binding.publication_id.clone()) {
            return Err(invalid_registry(format!(
                "publication id '{}' is bound to more than one workspace",
                binding.publication_id
            )));
        }
        let workspace = registry
            .workspaces
            .iter()
            .find(|workspace| workspace.id == binding.workspace_id)
            .ok_or_else(|| {
                invalid_registry(format!(
                    "publication binding references unknown workspace '{}'",
                    binding.workspace_id
                ))
            })?;
        publisher_allowed(registry, workspace, context.machine_id.as_deref()).map_err(
            |reason| {
                invalid_registry(format!(
                    "publication binding for workspace '{}': {reason}",
                    binding.workspace_id
                ))
            },
        )?;
        match workspace.git_remote.as_deref() {
            Some(git_remote) if git_remote == binding.source_repository_fingerprint => {}
            Some(_) => {
                return Err(invalid_registry(format!(
                    "workspace '{}' publication fingerprint does not match the registered source remote",
                    binding.workspace_id
                )));
            }
            None => {
                return Err(invalid_registry(format!(
                    "workspace '{}' publication binding has no registered source remote",
                    binding.workspace_id
                )));
            }
        }
        match workspace.owner_machine_id.as_deref() {
            Some(owner) if owner == binding.authority_machine_id => {}
            Some(owner) => {
                return Err(invalid_registry(format!(
                    "workspace '{}' publication authority '{}' does not match declared owner '{owner}'",
                    binding.workspace_id, binding.authority_machine_id
                )));
            }
            None => {
                return Err(invalid_registry(format!(
                    "workspace '{}' publication binding has no declared owner",
                    binding.workspace_id
                )));
            }
        }
        if let Some(local_machine_id) = context.machine_id.as_deref()
            && local_machine_id != binding.authority_machine_id
        {
            return Err(invalid_registry(format!(
                "workspace '{}' publication binding belongs to owner '{}', not local machine '{local_machine_id}'",
                binding.workspace_id, binding.authority_machine_id
            )));
        }
    }
    Ok(())
}

struct BindableWorkspace {
    id: String,
}

fn bindable_workspace(
    registry: &WorkspaceRegistry,
    id_or_name: &str,
    local_machine_id: Option<&str>,
) -> Result<BindableWorkspace, OrbitError> {
    if let Some(machine_id) = local_machine_id {
        validate_machine_id(machine_id)?;
    }
    let workspace = find_workspace(registry, id_or_name)?
        .ok_or_else(|| OrbitError::not_found(NotFoundKind::Workspace, id_or_name.to_string()))?;
    publisher_allowed(registry, workspace, local_machine_id).map_err(publication_error)?;
    Ok(BindableWorkspace {
        id: workspace.id.clone(),
    })
}

fn publisher_allowed(
    registry: &WorkspaceRegistry,
    workspace: &Workspace,
    local_machine_id: Option<&str>,
) -> Result<(), String> {
    if let Some(checkout) =
        find_checkout(registry, &workspace.id).map_err(|error| error.to_string())?
    {
        if checkout.role == Some(WorkspaceCheckoutRole::Replica) {
            return Err(format!(
                "workspace '{}' is a replica checkout; only the declared owner can manage a publication binding",
                workspace.id
            ));
        }
    } else {
        return Err(format!(
            "workspace '{}' has no local checkout; publication bindings are owner-local",
            workspace.id
        ));
    }
    let Some(owner) = workspace.owner_machine_id.as_deref() else {
        return Err(format!(
            "workspace '{}' has no declared owner; cannot bind a publication repository",
            workspace.id
        ));
    };
    if let Some(local_machine_id) = local_machine_id
        && local_machine_id != owner
    {
        return Err(format!(
            "workspace '{}' publication binding requires the declared owner machine '{owner}'",
            workspace.id
        ));
    }
    Ok(())
}

fn build_binding(
    registry: &WorkspaceRegistry,
    workspace_id: &str,
    publication_remote: &str,
    publication_branch: &str,
    publication_id: &str,
) -> Result<WorkspacePublicationBinding, OrbitError> {
    let workspace = find_workspace(registry, workspace_id)?
        .ok_or_else(|| OrbitError::not_found(NotFoundKind::Workspace, workspace_id.to_string()))?;
    let Some(fingerprint) = workspace.git_remote.as_deref() else {
        return Err(publication_error(format!(
            "workspace '{workspace_id}' has no registered source-repository identity"
        )));
    };
    let Some(authority) = workspace.owner_machine_id.as_deref() else {
        return Err(publication_error(format!(
            "workspace '{workspace_id}' has no declared owner; cannot bind a publication repository"
        )));
    };
    if let Some(existing) = registry.publication_bindings.iter().find(|binding| {
        binding.publication_id == publication_id && binding.workspace_id != workspace_id
    }) {
        return Err(publication_error(format!(
            "publication id '{publication_id}' is already bound to workspace '{}'",
            existing.workspace_id
        )));
    }
    let branch = canonicalize_publication_branch(publication_branch).map_err(workspace_error)?;
    validate_publication_remote(publication_remote).map_err(workspace_error)?;
    if git_remotes_equivalent(publication_remote, fingerprint).unwrap_or(false) {
        return Err(publication_error(format!(
            "publication remote '{}' is equivalent to the workspace source remote",
            redact_git_remote(publication_remote)
        )));
    }
    WorkspacePublicationBinding {
        workspace_id: workspace_id.to_string(),
        source_repository_fingerprint: fingerprint.to_string(),
        publication_remote: publication_remote.to_string(),
        publication_branch: branch,
        publication_id: publication_id.to_string(),
        authority_machine_id: authority.to_string(),
        last_success_generation: None,
        last_success_commit: None,
    }
    .validated()
    .map_err(workspace_error)
}

fn workspace_error(error: WorkspaceError) -> OrbitError {
    publication_error(error.to_string())
}

fn publication_error(message: String) -> OrbitError {
    OrbitError::WorkspaceError(message)
}

fn invalid_registry(message: String) -> OrbitError {
    OrbitError::WorkspaceError(format!("invalid registry: {message}"))
}
