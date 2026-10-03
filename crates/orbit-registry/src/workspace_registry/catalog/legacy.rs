//! Migration of the legacy path-bearing registry format to the catalog.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::workspace::{
    Workspace, WorkspaceCheckout, WorkspaceCheckoutRole, WorkspaceRegistry, WorkspaceStatus,
};
use serde::Deserialize;

use super::WorkspaceRegistryMachineContext;
use super::validation::invalid_registry;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyWorkspaceRegistry {
    #[serde(default)]
    workspaces: Vec<LegacyWorkspace>,
    #[serde(default)]
    path_overrides: BTreeMap<PathBuf, String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyWorkspace {
    id: String,
    name: String,
    root: PathBuf,
    orbit_dir: PathBuf,
    #[serde(default)]
    git_remote: Option<String>,
    #[serde(default)]
    ship_mode: Option<String>,
    #[serde(default = "legacy_default_base_branch")]
    base_branch: String,
    #[serde(default = "legacy_default_status")]
    status: WorkspaceStatus,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

pub(super) fn migrate_legacy_registry(
    content: &str,
    context: &WorkspaceRegistryMachineContext,
) -> Result<WorkspaceRegistry, OrbitError> {
    let legacy: LegacyWorkspaceRegistry = serde_json::from_str(content)
        .map_err(|error| invalid_registry(format!("invalid legacy registry: {error}")))?;
    let valid_ids: HashSet<String> = legacy
        .workspaces
        .iter()
        .map(|workspace| workspace.id.clone())
        .collect();
    let mut overrides_by_workspace: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for (path, workspace_id) in legacy.path_overrides {
        if valid_ids.contains(&workspace_id) {
            overrides_by_workspace
                .entry(workspace_id)
                .or_default()
                .push(path);
        }
    }

    let mut registry = WorkspaceRegistry::default();
    for legacy_workspace in legacy.workspaces {
        let workspace_id = legacy_workspace.id.clone();
        registry.workspaces.push(Workspace {
            id: legacy_workspace.id,
            name: legacy_workspace.name,
            owner_machine_id: context.machine_id.clone(),
            git_remote: legacy_workspace.git_remote,
            ship_mode: legacy_workspace.ship_mode,
            base_branch: legacy_workspace.base_branch,
            status: legacy_workspace.status,
            created_at: legacy_workspace.created_at,
            updated_at: legacy_workspace.updated_at,
        });
        registry.checkouts.push(WorkspaceCheckout {
            workspace_id: workspace_id.clone(),
            repo_root: legacy_workspace.root,
            orbit_dir: legacy_workspace.orbit_dir,
            // Only installations without a machine identity may use the legacy
            // owner default. Identity-bearing machines must declare a role.
            role: context
                .machine_id
                .is_none()
                .then_some(WorkspaceCheckoutRole::Owner),
            owner_machine_id: None,
            path_overrides: overrides_by_workspace
                .remove(&workspace_id)
                .unwrap_or_default(),
        });
    }
    Ok(registry)
}

fn legacy_default_base_branch() -> String {
    "main".to_string()
}

fn legacy_default_status() -> WorkspaceStatus {
    WorkspaceStatus::Active
}
