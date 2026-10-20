use clap::Args;
use orbit_core::OrbitRuntime;
use orbit_registry::workspace_registry;
use orbit_types::workspace::WorkspaceRegistry;
use serde_json::{Value, json};

use crate::command::{CommandOut, Execute, Payload};
use crate::output::table::{Column, Table};

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
        let registry = workspace_registry::load_validated_registry_from(
            &registry_path,
            workspace_registry::WorkspaceValidationMode::Persist,
        )?;
        let records = workspace_list_records(&registry, self.all);
        let table = workspace_list_table(&registry, self.all);
        Ok(Payload::list(records, table).into())
    }
}

fn matching_workspaces(
    registry: &WorkspaceRegistry,
    include_replicas: bool,
) -> impl Iterator<Item = &orbit_types::workspace::Workspace> {
    registry.workspaces.iter().filter(move |workspace| {
        include_replicas
            || registry
                .checkouts
                .iter()
                .find(|checkout| checkout.workspace_id == workspace.id)
                .is_none_or(|checkout| {
                    checkout.role != Some(orbit_types::workspace::WorkspaceCheckoutRole::Replica)
                })
    })
}

/// One record per registered workspace, carrying the same fields the text
/// columns show (ORB-10586).
fn workspace_list_records(registry: &WorkspaceRegistry, include_replicas: bool) -> Vec<Value> {
    matching_workspaces(registry, include_replicas)
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
        .collect()
}

fn workspace_list_table(registry: &WorkspaceRegistry, include_replicas: bool) -> Table {
    let mut table = Table::new(vec![
        Column::new("NAME"),
        Column::new("ID").fixed(),
        Column::new("STATUS").fixed(),
        Column::new("SHIP MODE").fixed(),
        Column::new("OWNER").fixed(),
        Column::new("ROLE").fixed(),
        Column::new("ROOT").path(),
    ])
    .empty_message("no workspaces registered");

    for workspace in matching_workspaces(registry, include_replicas) {
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
        table.add_row(vec![
            workspace.name.clone(),
            workspace.id.clone(),
            workspace.status.to_string(),
            orbit_core::resolved_ship_mode(workspace)
                .as_input_value()
                .to_string(),
            owner.to_string(),
            role,
            root,
        ]);
    }
    table
}
