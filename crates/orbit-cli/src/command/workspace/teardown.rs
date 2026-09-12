use std::path::{Path, PathBuf};

use clap::Args;
use orbit_cmd::{bound_partition_id, remove_checkout_task_stores, task_store_partition_path};
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_registry::workspace_registry;
use orbit_types::workspace::{Workspace, WorkspaceCheckout, WorkspaceRegistry};

use crate::command::{CommandOut, CommandOutput, Execute};

use super::support::{is_dir_empty, remove_symlinks_in};

#[derive(Args)]
pub struct WorkspaceTeardownArgs {
    /// Registered workspace name, logical id (`ws_*`), or absolute checkout path
    #[arg(value_name = "WORKSPACE", id = "workspace_selector")]
    pub workspace: String,

    /// Required flag to confirm destructive operation
    #[arg(long)]
    pub confirm: bool,
}

impl Execute for WorkspaceTeardownArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let global_root = runtime.global_root();
        let registry_path = workspace_registry::registry_path_for(&global_root);
        let registry = workspace_registry::load_registry_from(&registry_path)?;
        let (workspace, checkout) = resolve_teardown_target(&registry, &self.workspace)?;
        refuse_if_cwd_belongs_to_another_checkout(&registry, &checkout)?;

        let orbit_dir = checkout.orbit_dir.clone();
        let repo_root = checkout.repo_root.clone();
        let workspace_id = workspace.id.clone();
        let workspace_name = workspace.name.clone();

        let orbit_canonical =
            std::fs::canonicalize(&orbit_dir).unwrap_or_else(|_| orbit_dir.clone());
        let global_canonical =
            std::fs::canonicalize(&global_root).unwrap_or_else(|_| global_root.clone());
        if orbit_canonical == global_canonical {
            return Err(OrbitError::InvalidInput(
                "refusing to teardown the global ~/.orbit/ directory".to_string(),
            ));
        }
        if orbit_dir.file_name().and_then(|n| n.to_str()) != Some(".orbit") {
            return Err(OrbitError::InvalidInput(format!(
                "data root '{}' does not end with .orbit — aborting teardown",
                orbit_dir.display()
            )));
        }

        let partitions =
            planned_task_store_partitions(&global_root, &orbit_dir, Some(workspace_id.as_str()))?;
        let plan = format_teardown_plan(&workspace, &checkout, &partitions);
        if !self.confirm {
            return Err(OrbitError::InvalidInput(format!(
                "teardown is destructive. Resolved target:\n{plan}\nPass --confirm to proceed."
            )));
        }

        println!("teardown plan:\n{plan}");

        let mut removed: Vec<String> = Vec::new();

        // 1. Deregister from workspace registry (before deleting .orbit/)
        let mut catalog_workspace_id = None;
        if registry_path.exists() {
            let mut registry = workspace_registry::load_registry_from(&registry_path)?;
            if workspace_registry::find_workspace_by_id(&registry, &workspace_id).is_some() {
                let ws = workspace_registry::remove_workspace(&mut registry, &workspace_id)?;
                workspace_registry::save_registry_to(&registry, &registry_path)?;
                removed.push(format!(
                    "deregistered workspace '{}' from registry",
                    ws.name
                ));
                catalog_workspace_id = Some(workspace_id.clone());
            }
        }

        // 2. Delete the task-store partition this checkout's task state is
        //    bound to. The catalog id above is not that partition's name
        //    unless the two id spaces happen to coincide, so the task registry
        //    resolves it and retires its bindings [ORB-12119].
        for partition in
            remove_checkout_task_stores(&global_root, &orbit_dir, catalog_workspace_id.as_deref())?
        {
            removed.push(format_deleted_partition(&partition, &workspace_name));
        }

        // 3. Remove legacy repo-local skill symlinks from .agents/skills/ and .claude/skills/
        for dir_name in &[".agents", ".claude"] {
            let skills_dir = repo_root.join(dir_name).join("skills");
            if skills_dir.is_dir() {
                remove_symlinks_in(&skills_dir)?;
                removed.push(format!("removed symlinks from {}/skills/", dir_name));

                if is_dir_empty(&skills_dir) {
                    std::fs::remove_dir(&skills_dir).map_err(|e| OrbitError::Io(e.to_string()))?;
                }
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

fn resolve_teardown_target(
    registry: &WorkspaceRegistry,
    selector: &str,
) -> Result<(Workspace, WorkspaceCheckout), OrbitError> {
    let workspace_id = workspace_id_for_selector(registry, selector)?
        .ok_or_else(|| unknown_teardown_selector(selector))?;
    let workspace = workspace_registry::find_workspace_by_id(registry, &workspace_id)
        .cloned()
        .ok_or_else(|| unknown_teardown_selector(selector))?;
    let checkout = workspace_registry::find_checkout_by_id(registry, &workspace.id)
        .cloned()
        .ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "workspace '{}' ({}) has no registered checkout on this machine",
                workspace.name, workspace.id
            ))
        })?;
    Ok((workspace, checkout))
}

fn unknown_teardown_selector(selector: &str) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "unknown workspace selector '{selector}'; pass a registered workspace name, a logical workspace ID, or an absolute local checkout path"
    ))
}

fn workspace_id_for_selector(
    registry: &WorkspaceRegistry,
    selector: &str,
) -> Result<Option<String>, OrbitError> {
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

fn refuse_if_cwd_belongs_to_another_checkout(
    registry: &WorkspaceRegistry,
    selected: &WorkspaceCheckout,
) -> Result<(), OrbitError> {
    let cwd = std::env::current_dir().map_err(|e| OrbitError::Io(e.to_string()))?;
    let Some(cwd_checkout) = workspace_registry::find_checkout_by_path(registry, &cwd) else {
        return Ok(());
    };
    if cwd_checkout.workspace_id == selected.workspace_id {
        return Ok(());
    }

    let cwd_name = workspace_registry::find_workspace_by_id(registry, &cwd_checkout.workspace_id)
        .map(|workspace| workspace.name.as_str())
        .unwrap_or(cwd_checkout.workspace_id.as_str());
    Err(OrbitError::InvalidInput(format!(
        "workspace selector does not match the checkout containing the current directory ('{cwd_name}'); cd into the target checkout or pass that workspace's name, id, or absolute path"
    )))
}

fn planned_task_store_partitions(
    global_root: &Path,
    orbit_dir: &Path,
    catalog_workspace_id: Option<&str>,
) -> Result<Vec<PathBuf>, OrbitError> {
    let mut paths = Vec::new();
    if let Some(bound) = bound_partition_id(global_root, orbit_dir)? {
        let path = task_store_partition_path(global_root, &bound);
        if path.is_dir() {
            paths.push(path);
        }
    }
    if let Some(catalog_id) = catalog_workspace_id {
        let path = task_store_partition_path(global_root, catalog_id);
        if path.is_dir() && !paths.iter().any(|existing| existing == &path) {
            paths.push(path);
        }
    }
    Ok(paths)
}

pub(super) fn format_teardown_plan(
    workspace: &Workspace,
    checkout: &WorkspaceCheckout,
    partitions: &[PathBuf],
) -> String {
    let mut lines = vec![
        format!("workspace: {} ({})", workspace.name, workspace.id),
        format!("checkout: {}", checkout.repo_root.display()),
    ];
    if partitions.is_empty() {
        lines.push("task-store partition: (none)".to_string());
    } else {
        for path in partitions {
            let bundles = partition_bundle_count(path);
            lines.push(format!(
                "task-store partition: {} ({bundles} bundle{})",
                path.display(),
                if bundles == 1 { "" } else { "s" }
            ));
        }
    }
    lines.join("\n")
}

pub(super) fn format_deleted_partition(path: &Path, workspace_name: &str) -> String {
    let id = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("?");
    format!("deleted task store partition {id} (workspace '{workspace_name}')")
}

fn partition_bundle_count(path: &Path) -> usize {
    std::fs::read_dir(path)
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| entry.path().is_dir())
                .count()
        })
        .unwrap_or(0)
}
