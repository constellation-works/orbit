use std::path::Path;

use clap::Args;
use orbit_core::OrbitRuntime;
use orbit_registry::workspace_registry;
use orbit_types::workspace::WorkspaceRegistry;

use crate::command::{CommandOut, CommandOutput, Execute};

#[derive(Args)]
pub struct WorkspaceRemoveArgs {
    /// Workspace name, id, or absolute checkout path
    pub workspace: String,
}

impl Execute for WorkspaceRemoveArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let global_root = runtime.global_root();
        let registry_path = workspace_registry::registry_path_for(&global_root);
        let mut registry = workspace_registry::load_registry_from(&registry_path)?;
        let workspace_id = workspace_id_for_selector(&registry, &self.workspace)?;
        let removed = match workspace_id {
            Some(workspace_id) => {
                workspace_registry::remove_workspace(&mut registry, &workspace_id)?
            }
            None => workspace_registry::remove_workspace(&mut registry, &self.workspace)?,
        };
        workspace_registry::save_registry_to(&registry, &registry_path)?;
        println!("workspace '{}' removed from registry", removed.name);
        Ok(CommandOutput::Silent)
    }
}

fn workspace_id_for_selector(
    registry: &WorkspaceRegistry,
    selector: &str,
) -> Result<Option<String>, orbit_core::OrbitError> {
    if let Some(workspace) = workspace_registry::find_workspace(registry, selector)? {
        return Ok(Some(workspace.id.clone()));
    }

    if Path::new(selector).is_absolute() {
        return Ok(
            workspace_registry::find_checkout_by_path(registry, Path::new(selector))
                .map(|checkout| checkout.workspace_id.clone()),
        );
    }

    Ok(None)
}
