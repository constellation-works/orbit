use clap::Args;
use orbit_config::SeedingPolicy;
use orbit_core::Core;
use orbit_core::app::workspace::{Command, Outcome, Registered, Seeding, exec};
use orbit_types::workspace::{
    Workspace, WorkspaceCheckout, git_remote_identity, redact_git_remote,
};
use serde_json::{Value, json};

use crate::command::{CommandOut, ExecuteCore, Payload};

#[derive(Args)]
pub struct WorkspaceShowArgs {}

impl ExecuteCore for WorkspaceShowArgs {
    fn execute(self, core: &Core) -> CommandOut {
        let Outcome::Shown(shown) = exec(core, Command::Show)? else {
            unreachable!("workspace show returned an unrelated outcome");
        };
        match &shown.registered {
            Some(Registered {
                workspace,
                checkout,
                seeding,
            }) => Ok(Payload::detail(
                workspace_show_json(workspace, checkout, seeding),
                format_workspace_show(workspace, checkout, seeding),
            )
            .into()),
            // An unregistered root is still a describable state, not an error:
            // the record says which root it is and that it is not registered.
            None => Ok(Payload::detail(
                json!({
                    "orbit_root": shown.data_root.to_string_lossy(),
                    "registered": false,
                    "workspace": Value::Null,
                    "checkout": Value::Null,
                }),
                format!(
                    "current orbit root: {}\n(not registered as a workspace)",
                    shown.data_root.display()
                ),
            )
            .into()),
        }
    }
}

/// The seeding policy and the applied preset, as the record shows them.
fn seeding_json(seeding: &Seeding) -> (Value, Value) {
    (
        json!({
            "routines": seeding.policies.routines.to_json(),
            "auto_tasks": seeding.policies.auto_tasks.to_json(),
            "left_out": seeding.left_out,
        }),
        seeding
            .preset
            .as_ref()
            .map(|preset| {
                json!({
                    "name": preset.name,
                    "digest": preset.digest,
                    "applied_at": preset.applied_at,
                })
            })
            .unwrap_or(Value::Null),
    )
}

fn policy_label(policy: &SeedingPolicy) -> String {
    match policy {
        SeedingPolicy::All => "all".to_string(),
        SeedingPolicy::None => "none".to_string(),
        SeedingPolicy::Only(names) => names.iter().cloned().collect::<Vec<_>>().join(", "),
    }
}

/// `workspace show` had no machine-readable form; this is the record behind
/// the text below, added with the rest of the payload conversion (ORB-10586).
/// Both halves are built from the same two structs, so they cannot disagree.
fn workspace_show_json(
    workspace: &Workspace,
    checkout: &WorkspaceCheckout,
    seeding: &Seeding,
) -> Value {
    let (policy, preset) = seeding_json(seeding);
    json!({
        "orbit_root": checkout.orbit_dir.to_string_lossy(),
        "registered": true,
        "workspace": {
            "id": workspace.id,
            "name": workspace.name,
            "base_branch": workspace.base_branch,
            "ship_mode": orbit_core::app::run::resolved_ship_mode(workspace).as_input_value(),
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
        "seeding": policy,
        "preset": preset,
    })
}

fn format_workspace_show(
    workspace: &Workspace,
    checkout: &WorkspaceCheckout,
    seeding: &Seeding,
) -> String {
    let mut output = format!(
        "name:        {}\nid:          {}\nroot:        {}\norbit_dir:   {}\nbase_branch: {}\nship_mode:   {}\nstatus:      {}\n",
        workspace.name,
        workspace.id,
        checkout.repo_root.display(),
        checkout.orbit_dir.display(),
        workspace.base_branch,
        orbit_core::app::run::resolved_ship_mode(workspace).as_input_value(),
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
    output.push_str(&format!(
        "seeding:     routines={}; auto_tasks={}\n",
        policy_label(&seeding.policies.routines),
        policy_label(&seeding.policies.auto_tasks),
    ));
    if !seeding.policies.is_default() {
        output.push_str(&format!(
            "left_out:    {} default routine(s), {} default auto-task(s)\n",
            seeding.left_out.routines.len(),
            seeding.left_out.auto_tasks.len(),
        ));
    }
    if let Some(preset) = &seeding.preset {
        output.push_str(&format!(
            "preset:      {} ({}, applied {})\n",
            preset.name, preset.digest, preset.applied_at
        ));
    }
    output
}
