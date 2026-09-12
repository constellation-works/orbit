use clap::Args;
use orbit_core::OrbitRuntime;
use orbit_registry::workspace_registry;
use orbit_types::workspace::WorkspaceRegistry;
use serde_json::{Value, json};

use crate::command::{CommandOut, Execute, Payload};

#[derive(Args)]
pub struct WorkspaceListArgs {
    /// Include local replica checkouts, marked with their declared owner.
    #[arg(long)]
    pub all: bool,
}

impl Execute for WorkspaceListArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let global_root = runtime.global_root();
        let registry_path = workspace_registry::registry_path_for(&global_root);
        let registry = workspace_registry::with_registry_lock(&registry_path, || {
            let mut registry = workspace_registry::load_registry_from(&registry_path)?;
            if workspace_registry::validate_workspaces(&mut registry) {
                workspace_registry::save_registry_to(&registry, &registry_path)?;
            }
            Ok(registry)
        })?;
        Ok(Payload::detail(
            workspace_list_json(&registry, self.all),
            format_workspace_list(&registry, self.all),
        )
        .into())
    }
}

/// `workspace list` had no machine-readable form either; one record per
/// registered workspace, carrying the same fields the text columns show
/// (ORB-10586).
pub(super) fn workspace_list_json(registry: &WorkspaceRegistry, include_replicas: bool) -> Value {
    Value::Array(
        registry
            .workspaces
            .iter()
            .filter(|workspace| {
                include_replicas
                    || registry
                        .checkouts
                        .iter()
                        .find(|checkout| checkout.workspace_id == workspace.id)
                        .is_none_or(|checkout| checkout.role != Some(orbit_types::workspace::WorkspaceCheckoutRole::Replica))
            })
            .map(|workspace| {
                let checkout = registry
                    .checkouts
                    .iter()
                    .find(|checkout| checkout.workspace_id == workspace.id);
                json!({
                    "id": workspace.id,
                    "name": workspace.name,
                    "status": workspace.status.to_string(),
                    "ship_mode": orbit_core::resolved_ship_mode(workspace).as_input_value(),
                    "owner_machine_id": workspace.owner_machine_id,
                    "role": checkout.and_then(|checkout| checkout.role.map(|role| role.to_string())),
                    "repo_root": checkout.map(|checkout| checkout.repo_root.to_string_lossy()),
                })
            })
            .collect(),
    )
}

pub(super) fn format_workspace_list(
    registry: &WorkspaceRegistry,
    include_replicas: bool,
) -> String {
    let workspaces: Vec<_> = registry
        .workspaces
        .iter()
        .filter(|workspace| {
            include_replicas
                || registry
                    .checkouts
                    .iter()
                    .find(|checkout| checkout.workspace_id == workspace.id)
                    .is_none_or(|checkout| {
                        checkout.role
                            != Some(orbit_types::workspace::WorkspaceCheckoutRole::Replica)
                    })
        })
        .collect();
    if workspaces.is_empty() {
        return "no workspaces registered\n".to_string();
    }

    let id_width = workspaces
        .iter()
        .map(|workspace| workspace.id.chars().count())
        .max()
        .unwrap_or_default()
        .max("ID".len());
    let mut output = format!(
        "{:<20} {:<id_width$} {:<8} {:<10} {:<22} {:<8} ROOT\n",
        "NAME",
        "ID",
        "STATUS",
        "SHIP MODE",
        "OWNER",
        "ROLE",
        id_width = id_width
    );
    for workspace in workspaces {
        let checkout = registry
            .checkouts
            .iter()
            .find(|checkout| checkout.workspace_id == workspace.id);
        let root = checkout
            .map(|checkout| checkout.repo_root.display().to_string())
            .unwrap_or_else(|| "-".to_string());
        // Owner and local role are visible in multi-host output and render as
        // "-" in the standalone case where neither is declared.
        let owner = workspace.owner_machine_id.as_deref().unwrap_or("-");
        let role = checkout
            .and_then(|checkout| checkout.role)
            .map(|role| role.to_string())
            .unwrap_or_else(|| "-".to_string());
        output.push_str(&format!(
            "{:<20} {:<id_width$} {:<8} {:<10} {:<22} {:<8} {}\n",
            workspace.name,
            workspace.id,
            workspace.status.to_string(),
            orbit_core::resolved_ship_mode(workspace).as_input_value(),
            owner,
            role,
            root,
            id_width = id_width
        ));
    }
    output
}
