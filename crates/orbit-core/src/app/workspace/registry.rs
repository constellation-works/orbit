//! The host workspace registry commands, each on the workspace the `Core` is
//! bound to.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_registry::workspace_registry::{
    self, WorkspaceShipModeRebind, WorkspaceSourceRemoteRebind,
};
use orbit_registry::{MachineIdentityState, inspect_machine_identity, load_machine_identity};
use orbit_types::workspace::{
    Workspace, WorkspaceCheckout, WorkspaceCheckoutRole, WorkspacePublicationBinding,
    WorkspaceRegistry,
};

use super::{Registered, RoleAssignment, Seeding, Shown};
use crate::application::seeding_policy::{SeedingPolicyExclusions, workspace_seeding_policies};
use crate::application::workspace_preset::applied_workspace_preset;
use crate::{Core, ShipMode};

pub(super) fn list(core: &Core) -> Result<WorkspaceRegistry, OrbitError> {
    let registry_path = workspace_registry::registry_path_for(&core.runtime().global_root());
    workspace_registry::load_validated_registry_from(
        &registry_path,
        workspace_registry::WorkspaceValidationMode::Persist,
    )
}

pub(super) fn show(core: &Core) -> Result<Shown, OrbitError> {
    let runtime = core.runtime();
    let data_root = runtime.data_root();
    let global_root = runtime.global_root();
    let registry_path = workspace_registry::registry_path_for(&global_root);
    let registry = workspace_registry::load_registry_from(&registry_path)?;

    // The runtime factory resolves the current checkout from cwd (or an
    // explicit --workspace selector). `orbit_dir` is not checkout identity:
    // every checkout under an explicit shared root has the same value.
    let repo_root = runtime
        .workspace_runtime_binding()
        .map(|binding| binding.repo_root.as_path());
    let registered = match registered_workspace_for_repo_root(&registry, repo_root) {
        Some((workspace, checkout)) => {
            let seeding = seeding(&global_root, &checkout.orbit_dir)?;
            Some(Registered {
                workspace: workspace.clone(),
                checkout: checkout.clone(),
                seeding,
            })
        }
        None => None,
    };
    Ok(Shown {
        data_root,
        registered,
    })
}

fn seeding(global_root: &Path, orbit_dir: &Path) -> Result<Seeding, OrbitError> {
    let policies = workspace_seeding_policies(global_root, orbit_dir)?;
    Ok(Seeding {
        left_out: SeedingPolicyExclusions::of(&policies),
        policies,
        preset: applied_workspace_preset(orbit_dir)?,
    })
}

fn registered_workspace_for_repo_root<'a>(
    registry: &'a WorkspaceRegistry,
    repo_root: Option<&Path>,
) -> Option<(&'a Workspace, &'a WorkspaceCheckout)> {
    let checkout = repo_root.and_then(|repo_root| {
        registry
            .checkouts
            .iter()
            .find(|checkout| checkout.repo_root == repo_root)
    })?;
    registry
        .workspaces
        .iter()
        .find(|workspace| workspace.id == checkout.workspace_id)
        .map(|workspace| (workspace, checkout))
}

pub(super) fn source_remote(core: &Core) -> Result<Workspace, OrbitError> {
    let workspace_id = selected_workspace_id(core)?;
    let registry = workspace_registry::load_registry_from(&workspace_registry::registry_path_for(
        &core.runtime().global_root(),
    ))?;
    workspace_registry::find_workspace_by_id(&registry, &workspace_id)
        .cloned()
        .ok_or_else(|| OrbitError::WorkspaceError(format!("unknown workspace '{workspace_id}'")))
}

pub(super) fn rebind_source_remote(
    core: &Core,
    remote: &str,
    dry_run: bool,
) -> Result<WorkspaceSourceRemoteRebind, OrbitError> {
    let runtime = core.runtime();
    let workspace_id = selected_workspace_id(core)?;
    let global_root = runtime.global_root();
    let local_machine_id = load_machine_identity(&global_root)?.id;
    let task_workspace_id = selected_task_workspace_id(core)?;
    let registry_path = workspace_registry::registry_path_for(&global_root);
    workspace_registry::with_registry_lock(&registry_path, || {
        let mut registry = workspace_registry::load_registry_from(&registry_path)?;
        let outcome = workspace_registry::rebind_workspace_source_remote(
            &mut registry,
            &workspace_id,
            remote,
            Some(&local_machine_id),
            dry_run,
        )?;
        if outcome.changed && !outcome.dry_run {
            workspace_registry::save_registry_to(&registry, &registry_path)?;
        }
        // Save the authoritative registration first. An interrupted/failed
        // pairing write is recovered by repeating the same audited command,
        // even when the source registration itself is already unchanged.
        if !outcome.dry_run {
            runtime
                .rebind_task_publication_source(&task_workspace_id, &outcome.new_remote)
                .map_err(|error| {
                    OrbitError::WorkspaceError(format!(
                        "source remote is registered but its task-registry pairing could not be reconciled: {error}; repeat `orbit workspace source-remote rebind --remote <URL>` with the registered source URL"
                    ))
                })?;
        }
        Ok(outcome)
    })
}

pub(super) fn ship_mode(core: &Core) -> Result<(String, ShipMode), OrbitError> {
    let workspace_id = registered_workspace_id(core)?;
    let registry_path = workspace_registry::registry_path_for(&core.runtime().global_root());
    let registry = workspace_registry::load_registry_from(&registry_path)?;
    let workspace = workspace_registry::find_workspace_by_id(&registry, &workspace_id)
        .ok_or_else(|| OrbitError::WorkspaceError(format!("unknown workspace '{workspace_id}'")))?;
    let mode = crate::resolved_ship_mode(workspace);
    Ok((workspace_id, mode))
}

pub(super) fn rebind_ship_mode(
    core: &Core,
    mode: ShipMode,
) -> Result<WorkspaceShipModeRebind, OrbitError> {
    let workspace_id = registered_workspace_id(core)?;
    let registry_path = workspace_registry::registry_path_for(&core.runtime().global_root());
    workspace_registry::with_registry_lock(&registry_path, || {
        let mut registry = workspace_registry::load_registry_from(&registry_path)?;
        let outcome =
            workspace_registry::rebind_workspace_ship_mode(&mut registry, &workspace_id, mode)?;
        if outcome.changed {
            workspace_registry::save_registry_to(&registry, &registry_path)?;
        }
        Ok(outcome)
    })
}

/// The bound checkout's logical workspace id, which a ship-mode command
/// requires to be registered.
fn registered_workspace_id(core: &Core) -> Result<String, OrbitError> {
    core.runtime()
        .workspace_runtime_binding()
        .map(|binding| binding.logical_workspace_id.clone())
        .ok_or_else(|| {
            OrbitError::WorkspaceError(
                "this checkout is not a registered workspace; run `orbit workspace init` \
                 first, or select one with `--workspace`"
                    .to_string(),
            )
        })
}

/// Validate and persist one checkout role declaration.
pub(super) fn assign_role(
    core: &Core,
    workspace: &str,
    role: WorkspaceCheckoutRole,
    owner: Option<&str>,
) -> Result<RoleAssignment, OrbitError> {
    let global_root = core.runtime().global_root();
    let local_machine_id = match inspect_machine_identity(&global_root)? {
        MachineIdentityState::Present(identity) => Some(identity.id),
        MachineIdentityState::Absent => None,
    };
    let registry_path = workspace_registry::registry_path_for(&global_root);
    workspace_registry::with_registry_lock(&registry_path, || {
        let mut registry = workspace_registry::load_registry_from(&registry_path)?;

        workspace_registry::assign_checkout_role(
            &mut registry,
            workspace,
            role,
            owner,
            local_machine_id.as_deref(),
        )?;
        let (workspace_id, owner_machine_id) =
            workspace_registry::find_workspace(&registry, workspace)?
                .map(|found| (found.id.clone(), found.owner_machine_id.clone()))
                .unwrap_or_else(|| (workspace.to_string(), None));
        // save_registry_to validates a clone before writing, so a contradictory
        // declaration (owner role on a non-owner machine, replica of self, …)
        // fails here and leaves the previous registry file byte-valid.
        workspace_registry::save_registry_to(&registry, &registry_path)?;

        Ok(RoleAssignment {
            workspace_id,
            role,
            owner_machine_id,
        })
    })
}

pub(super) fn bind_publication(
    core: &Core,
    remote: &str,
    publication_id: &str,
    branch: &str,
    rebind: bool,
) -> Result<WorkspacePublicationBinding, OrbitError> {
    let runtime = core.runtime();
    let workspace_id = selected_workspace_id(core)?;
    let task_workspace_id = selected_task_workspace_id(core)?;
    let global_root = runtime.global_root();
    let machine_id = load_machine_identity(&global_root)?.id;
    let registry_path = workspace_registry::registry_path_for(&global_root);
    workspace_registry::with_registry_lock(&registry_path, || {
        let mut registry = workspace_registry::load_registry_from(&registry_path)?;
        let binding = if rebind {
            workspace_registry::rebind_publication_by_id(
                &mut registry,
                &workspace_id,
                remote,
                branch,
                publication_id,
                Some(&machine_id),
            )?
        } else {
            workspace_registry::bind_publication_by_id(
                &mut registry,
                &workspace_id,
                remote,
                branch,
                publication_id,
                Some(&machine_id),
            )?
        };
        runtime.record_task_publication_source(
            &task_workspace_id,
            &binding.source_repository_fingerprint,
        )?;
        workspace_registry::save_registry_to(&registry, &registry_path)?;

        Ok(binding)
    })
}

pub(super) fn publication(
    core: &Core,
) -> Result<(String, Option<WorkspacePublicationBinding>), OrbitError> {
    let workspace_id = selected_workspace_id(core)?;
    let registry = workspace_registry::load_registry_from(&workspace_registry::registry_path_for(
        &core.runtime().global_root(),
    ))?;
    let binding =
        workspace_registry::find_publication_binding_by_id(&registry, &workspace_id).cloned();
    Ok((workspace_id, binding))
}

pub(super) fn unbind_publication(core: &Core) -> Result<WorkspacePublicationBinding, OrbitError> {
    let workspace_id = selected_workspace_id(core)?;
    let global_root = core.runtime().global_root();
    let machine_id = load_machine_identity(&global_root)?.id;
    let registry_path = workspace_registry::registry_path_for(&global_root);
    workspace_registry::with_registry_lock(&registry_path, || {
        let mut registry = workspace_registry::load_registry_from(&registry_path)?;
        let removed = workspace_registry::unbind_publication_by_id(
            &mut registry,
            &workspace_id,
            Some(&machine_id),
        )?;
        workspace_registry::save_registry_to(&registry, &registry_path)?;
        Ok(removed)
    })
}

/// The bound checkout's logical workspace id, or the runtime's own.
fn selected_workspace_id(core: &Core) -> Result<String, OrbitError> {
    let runtime = core.runtime();
    runtime
        .workspace_runtime_binding()
        .map(|binding| binding.logical_workspace_id.clone())
        .map_or_else(|| runtime.workspace_id(), Ok)
}

/// The bound checkout's task partition, or the runtime's own workspace id.
fn selected_task_workspace_id(core: &Core) -> Result<String, OrbitError> {
    let runtime = core.runtime();
    runtime
        .workspace_runtime_binding()
        .map(|binding| binding.task_partition_id.clone())
        .map_or_else(|| runtime.workspace_id(), Ok)
}
