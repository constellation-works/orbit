//! Workspace and checkout mutations: registration, source-remote and
//! ship-mode rebinding, role assignment, removal, and path overrides.

use chrono::Utc;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::identity::validate_machine_id;
use orbit_types::workflow::{ShipMode, resolved_ship_mode};
use orbit_types::workspace::{
    Workspace, WorkspaceCheckout, WorkspaceCheckoutRole, WorkspaceRegistry, git_remote_identity,
    git_remotes_equivalent, redact_git_remote, validate_source_repository_fingerprint,
};
use std::path::PathBuf;

use super::find_workspace;

/// Registers a new workspace. Errors if a workspace with the same id or name already exists.
pub fn register_workspace(
    registry: &mut WorkspaceRegistry,
    ws: Workspace,
) -> Result<(), OrbitError> {
    if registry.workspaces.iter().any(|w| w.id == ws.id) {
        return Err(OrbitError::WorkspaceError(format!(
            "workspace with id '{}' already exists",
            ws.id
        )));
    }
    if registry.workspaces.iter().any(|w| w.name == ws.name) {
        return Err(OrbitError::WorkspaceError(format!(
            "workspace with name '{}' already exists",
            ws.name
        )));
    }
    registry.workspaces.push(ws);
    Ok(())
}

/// Registers a machine-local checkout for an existing logical workspace.
pub fn register_checkout(
    registry: &mut WorkspaceRegistry,
    checkout: WorkspaceCheckout,
) -> Result<(), OrbitError> {
    if !registry
        .workspaces
        .iter()
        .any(|workspace| workspace.id == checkout.workspace_id)
    {
        return Err(OrbitError::not_found(
            NotFoundKind::Workspace,
            checkout.workspace_id,
        ));
    }
    if registry
        .checkouts
        .iter()
        .any(|existing| existing.workspace_id == checkout.workspace_id)
    {
        return Err(OrbitError::WorkspaceError(format!(
            "local checkout for workspace '{}' already exists",
            checkout.workspace_id
        )));
    }
    if let Some(existing) = registry
        .checkouts
        .iter()
        .find(|existing| existing.repo_root == checkout.repo_root)
    {
        return Err(OrbitError::WorkspaceError(format!(
            "checkout path '{}' is already registered to workspace '{}'",
            checkout.repo_root.display(),
            existing.workspace_id
        )));
    }
    registry.checkouts.push(checkout);
    Ok(())
}

/// The inspected result of explicitly rebinding one workspace's source remote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceSourceRemoteRebind {
    pub workspace_id: String,
    pub old_remote: String,
    pub old_repository_identity: Option<String>,
    pub new_remote: String,
    pub new_repository_identity: String,
    pub changed: bool,
    pub dry_run: bool,
}

/// Reconcile a detected origin during explicit initialization of an existing workspace.
///
/// Only the declared local owner can establish a missing source identity.
/// Exact matches preserve even non-portable registered origins. Equivalent portable
/// origins preserve the registered URL (and publication fingerprint); changing a
/// registered source requires the audited rebind command.
/// All validation precedes mutation. An absent origin leaves the identity intact.
pub fn reconcile_workspace_source_remote(
    registry: &mut WorkspaceRegistry,
    workspace_id: &str,
    detected_remote: Option<&str>,
    local_machine_id: Option<&str>,
) -> Result<(), OrbitError> {
    let Some(remote) = detected_remote else {
        return Ok(());
    };
    let workspace = super::find_workspace_by_id(registry, workspace_id)
        .ok_or_else(|| OrbitError::not_found(NotFoundKind::Workspace, workspace_id.to_string()))?;
    if let Some(registered_remote) = workspace.git_remote.as_deref() {
        if registered_remote == remote
            || (validate_source_repository_fingerprint(registered_remote).is_ok()
                && validate_source_repository_fingerprint(remote).is_ok()
                && git_remotes_equivalent(registered_remote, remote).unwrap_or(false))
        {
            return Ok(());
        }
        return Err(OrbitError::WorkspaceError(format!(
            "workspace '{workspace_id}' already has a different registered source remote; inspect it with `orbit --workspace {workspace_id} workspace source-remote show --json`, then use `orbit --workspace {workspace_id} workspace source-remote rebind --remote <URL>` on the declared owner machine"
        )));
    }
    validate_source_repository_fingerprint(remote)
        .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
    validate_source_remote_owner(registry, workspace, local_machine_id)?;
    let workspace = registry
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.id == workspace_id)
        .ok_or_else(|| OrbitError::not_found(NotFoundKind::Workspace, workspace_id.to_string()))?;
    workspace.git_remote = Some(remote.to_string());
    workspace.updated_at = Utc::now();
    Ok(())
}

/// The result of rebinding one workspace's registered ship mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceShipModeRebind {
    pub workspace_id: String,
    /// The effective mode before the rebind.
    pub previous: ShipMode,
    /// The stored value before the rebind; `None` when it was unset, which
    /// resolves to `pr`.
    pub previous_stored: Option<String>,
    pub ship_mode: ShipMode,
    /// Whether the stored value changed. Rebinding an unset mode to `pr`
    /// stores it explicitly without changing the effective mode.
    pub changed: bool,
}

/// Set a workspace's registered ship mode, touching nothing else on the
/// record but `updated_at`. Managed defaults and config files are not this
/// registry's, so they are never read or written here.
pub fn rebind_workspace_ship_mode(
    registry: &mut WorkspaceRegistry,
    id_or_name: &str,
    ship_mode: ShipMode,
) -> Result<WorkspaceShipModeRebind, OrbitError> {
    let workspace_id = find_workspace(registry, id_or_name)?
        .ok_or_else(|| OrbitError::not_found(NotFoundKind::Workspace, id_or_name.to_string()))?
        .id
        .clone();
    let workspace = registry
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.id == workspace_id)
        .ok_or_else(|| OrbitError::not_found(NotFoundKind::Workspace, workspace_id.clone()))?;
    let previous = resolved_ship_mode(workspace);
    let previous_stored = workspace.ship_mode.clone();
    let stored = ship_mode.as_input_value();
    let changed = previous_stored.as_deref() != Some(stored);
    if changed {
        workspace.ship_mode = Some(stored.to_string());
        workspace.updated_at = Utc::now();
    }
    Ok(WorkspaceShipModeRebind {
        workspace_id,
        previous,
        previous_stored,
        ship_mode,
        changed,
    })
}

/// Rebind an owned workspace's portable source-repository identity.
///
/// Every refusal happens before the registry is mutated. Publication bindings
/// deliberately fail closed because their source fingerprint is part of the
/// published lineage; the operator must remove and recreate that binding
/// explicitly around a repository move.
pub fn rebind_workspace_source_remote(
    registry: &mut WorkspaceRegistry,
    id_or_name: &str,
    new_remote: &str,
    local_machine_id: Option<&str>,
    dry_run: bool,
) -> Result<WorkspaceSourceRemoteRebind, OrbitError> {
    validate_source_repository_fingerprint(new_remote)
        .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
    let new_repository_identity = git_remote_identity(new_remote)
        .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;

    let workspace = find_workspace(registry, id_or_name)?
        .ok_or_else(|| OrbitError::not_found(NotFoundKind::Workspace, id_or_name.to_string()))?;
    let workspace_id = workspace.id.clone();
    let old_remote = workspace.git_remote.clone().ok_or_else(|| {
        OrbitError::WorkspaceError(format!(
            "workspace '{workspace_id}' has no registered source remote; set a portable Git origin on the declared owner's checkout, then run `orbit workspace init --name {} --force` there to bind its first source identity",
            workspace.name
        ))
    })?;
    let old_repository_identity = git_remote_identity(&old_remote).ok();
    let old_remote_is_portable = validate_source_repository_fingerprint(&old_remote).is_ok();

    validate_source_remote_owner(registry, workspace, local_machine_id)?;

    let changed = !old_remote_is_portable
        || old_repository_identity.as_deref() != Some(&new_repository_identity);
    let outcome = WorkspaceSourceRemoteRebind {
        workspace_id: workspace_id.clone(),
        old_remote,
        old_repository_identity,
        new_remote: new_remote.to_string(),
        new_repository_identity,
        changed,
        dry_run,
    };
    if !changed {
        return Ok(outcome);
    }

    if let Some(binding) = registry
        .publication_bindings
        .iter()
        .find(|binding| binding.workspace_id == workspace_id)
    {
        return Err(OrbitError::WorkspaceError(format!(
            "workspace '{workspace_id}' has publication binding '{}' tied to source '{}'; refusing to rewrite publication lineage. Inspect it with `orbit --workspace {workspace_id} workspace publication show --json`, record its settings, remove it with `orbit --workspace {workspace_id} workspace publication remove --confirm`, perform the source-remote rebind, then create a new binding explicitly",
            binding.publication_id,
            redact_git_remote(&binding.source_repository_fingerprint),
        )));
    }
    if dry_run {
        return Ok(outcome);
    }

    let workspace = registry
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.id == workspace_id)
        .ok_or_else(|| OrbitError::not_found(NotFoundKind::Workspace, workspace_id.clone()))?;
    workspace.git_remote = Some(new_remote.to_string());
    workspace.updated_at = Utc::now();

    Ok(outcome)
}

fn validate_source_remote_owner(
    registry: &WorkspaceRegistry,
    workspace: &Workspace,
    local_machine_id: Option<&str>,
) -> Result<(), OrbitError> {
    let local_machine_id = local_machine_id.ok_or_else(|| {
        OrbitError::WorkspaceError(
            "source-remote binding requires a local machine identity; run `orbit init` first"
                .to_string(),
        )
    })?;
    validate_machine_id(local_machine_id)?;
    let workspace_id = &workspace.id;
    let checkout = registry
        .checkouts
        .iter()
        .find(|checkout| checkout.workspace_id == *workspace_id)
        .ok_or_else(|| {
            OrbitError::WorkspaceError(format!(
                "workspace '{workspace_id}' has no local checkout; source-remote rebinding is owner-local"
            ))
        })?;
    match checkout.role {
        Some(WorkspaceCheckoutRole::Owner) => {}
        Some(WorkspaceCheckoutRole::Replica) => {
            return Err(OrbitError::WorkspaceError(format!(
                "workspace '{workspace_id}' is a replica checkout; bind the source remote on its declared owner machine"
            )));
        }
        None => {
            return Err(OrbitError::WorkspaceError(format!(
                "workspace '{workspace_id}' has no declared local checkout role; reassert its owner role before rebinding the source remote"
            )));
        }
    }
    let owner_machine_id = workspace.owner_machine_id.as_deref().ok_or_else(|| {
        OrbitError::WorkspaceError(format!(
            "workspace '{workspace_id}' has no declared owner; reassert its owner role before rebinding the source remote"
        ))
    })?;
    if owner_machine_id != local_machine_id {
        return Err(OrbitError::WorkspaceError(format!(
            "workspace '{workspace_id}' is owned by machine '{owner_machine_id}'; local machine '{local_machine_id}' cannot rebind its source remote"
        )));
    }

    Ok(())
}

/// Record a machine-local checkout role for an existing logical workspace.
///
/// `Owner` requires no replica owner and leaves the logical owner to be
/// declared explicitly from the validated local `machine_id`. `Replica`
/// requires an explicit non-local `owner_machine_id`, which is mirrored onto
/// both the checkout binding and the logical workspace record so the stable
/// owner identity stays consistent. The optional local identity exists only
/// for pre-machine-identity standalone compatibility. This mutates the in-memory
/// registry only; the caller persists via [`super::super::save_registry_to`], which validates a clone and
/// therefore leaves the previous file byte-valid on any contradiction. Owner
/// and replica declarations are never inferred from paths, workspace names,
/// presence, hostnames, or Git remotes.
pub fn assign_checkout_role(
    registry: &mut WorkspaceRegistry,
    id_or_name: &str,
    role: WorkspaceCheckoutRole,
    owner_machine_id: Option<&str>,
    local_machine_id: Option<&str>,
) -> Result<(), OrbitError> {
    let workspace = find_workspace(registry, id_or_name)?
        .ok_or_else(|| OrbitError::not_found(NotFoundKind::Workspace, id_or_name.to_string()))?;
    let workspace_id = workspace.id.clone();
    let declared_owner = workspace.owner_machine_id.clone();
    let checkout_index = registry
        .checkouts
        .iter()
        .position(|checkout| checkout.workspace_id == workspace_id)
        .ok_or_else(|| {
            OrbitError::WorkspaceError(format!(
                "workspace '{workspace_id}' has no local checkout binding"
            ))
        })?;

    match role {
        WorkspaceCheckoutRole::Owner => {
            if owner_machine_id.is_some() {
                return Err(OrbitError::WorkspaceError(format!(
                    "workspace '{workspace_id}' owner role does not take an owner machine_id; drop \
                     `--owner` (only `replica` names an owner)"
                )));
            }
            if let Some(local_machine_id) = local_machine_id {
                validate_machine_id(local_machine_id)?;
                if let Some(existing_owner) = declared_owner.as_deref()
                    && existing_owner != local_machine_id
                {
                    return Err(OrbitError::WorkspaceError(format!(
                        "workspace '{workspace_id}' is declared owned by machine \
                         '{existing_owner}'; local machine '{local_machine_id}' cannot assign \
                         itself the owner role"
                    )));
                }
                if declared_owner.is_none()
                    && let Some(workspace) = registry
                        .workspaces
                        .iter_mut()
                        .find(|workspace| workspace.id == workspace_id)
                {
                    workspace.owner_machine_id = Some(local_machine_id.to_string());
                }
            }
        }
        WorkspaceCheckoutRole::Replica => {
            let owner = owner_machine_id.ok_or_else(|| {
                OrbitError::WorkspaceError(format!(
                    "workspace '{workspace_id}' replica role requires an owner machine_id; pass \
                     `--owner <machine_id>` with the owner's `machine.id` (run `orbit config get \
                     machine.id` on the owner host)"
                ))
            })?;
            validate_machine_id(owner).map_err(|error| {
                OrbitError::InvalidInput(format!(
                    "owner machine_id '{owner}' is not usable ({error}); pass the owner host's \
                     `machine.id` as `--owner <machine_id>` (`orbit config get machine.id` there)"
                ))
            })?;
            if local_machine_id == Some(owner) {
                return Err(OrbitError::WorkspaceError(format!(
                    "workspace '{workspace_id}' cannot declare the local machine '{owner}' as \
                     its replica owner; `--owner` must name the other host that owns the \
                     workspace (use the owner role on the owner's own checkout)"
                )));
            }
            if let Some(existing_owner) = declared_owner.as_deref()
                && existing_owner != owner
            {
                return Err(OrbitError::WorkspaceError(format!(
                    "workspace '{workspace_id}' is already owned by machine '{existing_owner}'; \
                     refusing to rebind it to '{owner}' while assigning a replica role. To demote \
                     an owner checkout, run `ORBIT_OPERATOR=1 orbit workspace remove \
                     {workspace_id}` (registry only; `.orbit` is kept), then `orbit workspace init \
                     --role replica --owner {owner}`"
                )));
            }
            if let Some(existing_owner) = registry.checkouts[checkout_index]
                .owner_machine_id
                .as_deref()
                && existing_owner != owner
            {
                return Err(OrbitError::WorkspaceError(format!(
                    "workspace '{workspace_id}' checkout already mirrors owner machine \
                     '{existing_owner}'; refusing contradictory owner '{owner}'"
                )));
            }
            if let Some(workspace) = registry
                .workspaces
                .iter_mut()
                .find(|workspace| workspace.id == workspace_id)
            {
                workspace.owner_machine_id = Some(owner.to_string());
            }
        }
    }

    let checkout = &mut registry.checkouts[checkout_index];
    checkout.role = Some(role);
    checkout.owner_machine_id = match role {
        WorkspaceCheckoutRole::Owner => None,
        WorkspaceCheckoutRole::Replica => owner_machine_id.map(str::to_string),
    };
    Ok(())
}

/// Removes a workspace by id or name. Returns the removed workspace.
pub fn remove_workspace(
    registry: &mut WorkspaceRegistry,
    id_or_name: &str,
) -> Result<Workspace, OrbitError> {
    let workspace_id = find_workspace(registry, id_or_name)?
        .ok_or_else(|| OrbitError::not_found(NotFoundKind::Workspace, id_or_name.to_string()))?
        .id
        .clone();
    let idx = registry
        .workspaces
        .iter()
        .position(|workspace| workspace.id == workspace_id)
        .ok_or_else(|| OrbitError::not_found(NotFoundKind::Workspace, id_or_name.to_string()))?;
    let removed = registry.workspaces.remove(idx);
    registry
        .checkouts
        .retain(|checkout| checkout.workspace_id != removed.id);
    registry
        .publication_bindings
        .retain(|binding| binding.workspace_id != removed.id);
    Ok(removed)
}

/// Sets a path override binding a directory to a workspace.
///
/// The path must not already be another checkout's `repo_root` or override.
/// Persistence repeats this uniqueness rule so a hand-edited or CLI-rewritten
/// registry cannot save a collision that would make `find_checkout_by_path`
/// order-dependent.
pub fn set_path_override(
    registry: &mut WorkspaceRegistry,
    path: PathBuf,
    workspace_id: &str,
) -> Result<(), OrbitError> {
    let checkout_index = registry
        .checkouts
        .iter()
        .position(|checkout| checkout.workspace_id == workspace_id)
        .ok_or_else(|| {
            OrbitError::WorkspaceError(format!(
                "workspace '{workspace_id}' has no local checkout binding"
            ))
        })?;
    if let Some(existing) = registry
        .checkouts
        .iter()
        .enumerate()
        .find(|(index, checkout)| {
            *index != checkout_index
                && (checkout.repo_root == path
                    || checkout
                        .path_overrides
                        .iter()
                        .any(|claimed| claimed == &path))
        })
        .map(|(_, checkout)| checkout.workspace_id.as_str())
    {
        return Err(OrbitError::WorkspaceError(format!(
            "checkout path '{}' is already registered to workspace '{existing}'",
            path.display()
        )));
    }
    let checkout = &mut registry.checkouts[checkout_index];
    if !checkout.path_overrides.contains(&path) {
        checkout.path_overrides.push(path);
        checkout.path_overrides.sort();
    }
    Ok(())
}
