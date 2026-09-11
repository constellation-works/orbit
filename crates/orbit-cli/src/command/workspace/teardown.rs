use clap::Args;
use orbit_cmd::remove_checkout_task_stores;
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_registry::workspace_registry;

use crate::command::{CommandOut, CommandOutput, Execute};

use super::support::{is_dir_empty, remove_symlinks_in};

#[derive(Args)]
pub struct WorkspaceTeardownArgs {
    /// Required flag to confirm destructive operation
    #[arg(long)]
    pub confirm: bool,
}

impl Execute for WorkspaceTeardownArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        if !self.confirm {
            return Err(OrbitError::InvalidInput(
                "teardown is destructive. Pass --confirm to proceed.".to_string(),
            ));
        }

        let orbit_dir = runtime.data_root();
        let global_dir = runtime.global_root();

        // Safety: never delete the global ~/.orbit/ directory
        let orbit_canonical =
            std::fs::canonicalize(&orbit_dir).unwrap_or_else(|_| orbit_dir.clone());
        let global_canonical =
            std::fs::canonicalize(&global_dir).unwrap_or_else(|_| global_dir.clone());
        if orbit_canonical == global_canonical {
            return Err(OrbitError::InvalidInput(
                "refusing to teardown the global ~/.orbit/ directory".to_string(),
            ));
        }

        // Safety: orbit_dir must end with ".orbit"
        if orbit_dir.file_name().and_then(|n| n.to_str()) != Some(".orbit") {
            return Err(OrbitError::InvalidInput(format!(
                "data root '{}' does not end with .orbit — aborting teardown",
                orbit_dir.display()
            )));
        }

        let repo_root = orbit_dir
            .parent()
            .ok_or_else(|| OrbitError::InvalidInput("cannot determine repo root".to_string()))?;

        let mut removed: Vec<String> = Vec::new();

        // 1. Deregister from workspace registry (before deleting .orbit/)
        let global_root = runtime.global_root();
        let registry_path = workspace_registry::registry_path_for(&global_root);
        let mut catalog_workspace_id = None;
        if registry_path.exists() {
            let mut registry = workspace_registry::load_registry_from(&registry_path)?;
            let checkout = registry.checkouts.iter().find(|checkout| {
                let ws_canonical = std::fs::canonicalize(&checkout.orbit_dir)
                    .unwrap_or_else(|_| checkout.orbit_dir.clone());
                ws_canonical == orbit_canonical
            });
            if let Some(ws_id) = checkout.map(|checkout| checkout.workspace_id.clone()) {
                let ws = workspace_registry::remove_workspace(&mut registry, &ws_id)?;
                workspace_registry::save_registry_to(&registry, &registry_path)?;
                removed.push(format!(
                    "deregistered workspace '{}' from registry",
                    ws.name
                ));
                catalog_workspace_id = Some(ws_id);
            }
        }

        // 2. Delete the task-store partition this checkout's task state is
        //    bound to. The catalog id above is not that partition's name
        //    unless the two id spaces happen to coincide, so the task registry
        //    resolves it and retires its bindings [ORB-12119].
        for partition in
            remove_checkout_task_stores(&global_root, &orbit_dir, catalog_workspace_id.as_deref())?
        {
            removed.push(format!("deleted task store {}", partition.display()));
        }

        // 3. Remove legacy repo-local skill symlinks from .agents/skills/ and .claude/skills/
        for dir_name in &[".agents", ".claude"] {
            let skills_dir = repo_root.join(dir_name).join("skills");
            if skills_dir.is_dir() {
                remove_symlinks_in(&skills_dir)?;
                removed.push(format!("removed symlinks from {}/skills/", dir_name));

                // Remove skills dir if empty
                if is_dir_empty(&skills_dir) {
                    std::fs::remove_dir(&skills_dir).map_err(|e| OrbitError::Io(e.to_string()))?;
                }
                // Remove parent dir if empty
                let parent = repo_root.join(dir_name);
                if parent.is_dir() && is_dir_empty(&parent) {
                    std::fs::remove_dir(&parent).map_err(|e| OrbitError::Io(e.to_string()))?;
                    removed.push(format!("removed empty {}/", dir_name));
                }
            }
        }

        // 4. Delete .orbit/ directory
        if orbit_dir.is_dir() {
            std::fs::remove_dir_all(&orbit_dir).map_err(|e| OrbitError::Io(e.to_string()))?;
            removed.push(format!("deleted {}", orbit_dir.display()));
        }

        // 5. Print summary
        println!("teardown complete:");
        for item in &removed {
            println!("  - {item}");
        }
        if removed.is_empty() {
            println!("  (nothing to remove)");
        }

        Ok(CommandOutput::Silent)
    }
}
