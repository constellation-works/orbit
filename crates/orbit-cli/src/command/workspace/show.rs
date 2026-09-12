use std::path::Path;

use clap::Args;
use orbit_core::OrbitRuntime;
use orbit_registry::workspace_registry;
use orbit_types::workspace::{
    Workspace, WorkspaceCheckout, WorkspaceRegistry, git_remote_identity, redact_git_remote,
};
use serde_json::{Value, json};

use crate::command::{CommandOut, Execute, Payload};

#[derive(Args)]
pub struct WorkspaceShowArgs {}

impl Execute for WorkspaceShowArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
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

        match registered_workspace_for_repo_root(&registry, repo_root) {
            Some((workspace, checkout)) => Ok(Payload::detail(
                workspace_show_json(workspace, checkout),
                format_workspace_show(workspace, checkout),
            )
            .into()),
            // An unregistered root is still a describable state, not an error:
            // the record says which root it is and that it is not registered.
            None => Ok(Payload::detail(
                json!({
                    "orbit_root": data_root.to_string_lossy(),
                    "registered": false,
                    "workspace": Value::Null,
                    "checkout": Value::Null,
                }),
                format!(
                    "current orbit root: {}\n(not registered as a workspace)",
                    data_root.display()
                ),
            )
            .into()),
        }
    }
}

pub(super) fn registered_workspace_for_repo_root<'a>(
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

/// `workspace show` had no machine-readable form; this is the record behind
/// the text below, added with the rest of the payload conversion (ORB-10586).
/// Both halves are built from the same two structs, so they cannot disagree.
pub(super) fn workspace_show_json(workspace: &Workspace, checkout: &WorkspaceCheckout) -> Value {
    json!({
        "orbit_root": checkout.orbit_dir.to_string_lossy(),
        "registered": true,
        "workspace": {
            "id": workspace.id,
            "name": workspace.name,
            "base_branch": workspace.base_branch,
            "ship_mode": orbit_core::resolved_ship_mode(workspace).as_input_value(),
            "status": workspace.status.to_string(),
            "owner_machine_id": workspace.owner_machine_id,
            "git_remote": workspace.git_remote.as_deref().map(redact_git_remote),
            "source_repository_identity": workspace
                .git_remote
                .as_deref()
                .and_then(|remote| git_remote_identity(remote).ok()),
        },
        "checkout": {
            "repo_root": checkout.repo_root.to_string_lossy(),
            "orbit_dir": checkout.orbit_dir.to_string_lossy(),
            "role": checkout.role.map(|role| role.to_string()),
            "owner_machine_id": checkout
                .owner_machine_id
                .as_ref()
                .or(workspace.owner_machine_id.as_ref()),
        },
    })
}

pub(super) fn format_workspace_show(workspace: &Workspace, checkout: &WorkspaceCheckout) -> String {
    let mut output = format!(
        "name:        {}\nid:          {}\nroot:        {}\norbit_dir:   {}\nbase_branch: {}\nship_mode:   {}\nstatus:      {}\n",
        workspace.name,
        workspace.id,
        checkout.repo_root.display(),
        checkout.orbit_dir.display(),
        workspace.base_branch,
        orbit_core::resolved_ship_mode(workspace).as_input_value(),
        workspace.status,
    );
    output.push_str(&format!(
        "owner:       {}\nrole:        {}\n",
        workspace.owner_machine_id.as_deref().unwrap_or("-"),
        checkout
            .role
            .map(|role| role.to_string())
            .unwrap_or_else(|| "-".to_string()),
    ));
    if let Some(replica_owner) = &checkout.owner_machine_id {
        output.push_str(&format!("owner_mirror: {replica_owner}\n"));
    }
    if let Some(remote) = &workspace.git_remote {
        output.push_str(&format!("git_remote:  {}\n", redact_git_remote(remote)));
        if let Ok(identity) = git_remote_identity(remote) {
            output.push_str(&format!("source_id:   {identity}\n"));
        }
    }
    output.push_str(&format!(
        "created_at:  {}\nupdated_at:  {}\n",
        workspace.created_at, workspace.updated_at
    ));
    output
}
