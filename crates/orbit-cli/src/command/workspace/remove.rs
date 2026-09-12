use std::path::Path;

use clap::Args;
use orbit_cmd::retain_task_store_on_catalog_remove;
use orbit_core::OrbitRuntime;
use orbit_registry::workspace_registry;
use orbit_types::workspace::WorkspaceRegistry;

use crate::command::{CommandOut, CommandOutput, Execute};

#[derive(Args)]
pub struct WorkspaceRemoveArgs {
    /// Workspace name, id, or absolute checkout path
    #[arg(value_name = "WORKSPACE", id = "workspace_selector")]
    pub workspace: String,
}

impl Execute for WorkspaceRemoveArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let global_root = runtime.global_root();
        let registry_path = workspace_registry::registry_path_for(&global_root);
        let mut registry = workspace_registry::load_registry_from(&registry_path)?;
        let workspace_id = workspace_id_for_selector(&registry, &self.workspace)?
            .unwrap_or_else(|| self.workspace.clone());
        let checkout = registry
            .checkouts
            .iter()
            .find(|checkout| checkout.workspace_id == workspace_id)
            .cloned();
        let slug = workspace_registry::find_workspace_by_id(&registry, &workspace_id)
            .map(|workspace| workspace.name.clone())
            .unwrap_or_else(|| workspace_id.clone());

        // Retain checkout evidence before the catalog rows disappear so doctor
        // can still classify a leftover populated partition [ORB-12223].
        let leftover = retain_task_store_on_catalog_remove(
            &global_root,
            &workspace_id,
            &slug,
            checkout.as_ref(),
        )?;

        let removed = workspace_registry::remove_workspace(&mut registry, &workspace_id)?;
        workspace_registry::save_registry_to(&registry, &registry_path)?;
        println!("workspace '{}' removed from registry", removed.name);
        if let Some(partition) = leftover.filter(|partition| partition.task_bundles > 0) {
            println!(
                "left task-store partition {} ({} task bundle(s)); \
                 the task-registry workspace binding is retained as checkout evidence \
                 so `orbit doctor` can classify it. Reclaim with \
                 `orbit doctor --fix-orphan-task-stores --confirm`.",
                partition.path.display(),
                partition.task_bundles
            );
        }
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
