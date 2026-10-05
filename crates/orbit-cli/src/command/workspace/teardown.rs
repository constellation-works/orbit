use std::path::{Path, PathBuf};

use clap::Args;
use orbit_cmd::{checkout_task_store_partitions, remove_checkout_task_stores};
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_registry::workspace_registry;
use orbit_types::workspace::{Workspace, WorkspaceCheckout, WorkspaceRegistry};

use serde_json::json;

use crate::command::{CommandOut, Execute, Payload};

use super::support::{LEGACY_SKILL_DISCOVERY_DIRS, remove_owned_skill_links};

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
        let cwd = std::env::current_dir().map_err(|e| OrbitError::Io(e.to_string()))?;
        self.execute_from(runtime, &cwd)
    }
}

impl WorkspaceTeardownArgs {
    /// Run teardown as if invoked from `cwd`.
    ///
    /// The working directory only feeds the "wrong checkout" refusal, so it is
    /// an explicit input: the process cwd is global state, and tests must not
    /// have to mutate (or race on) it to exercise teardown.
    pub(super) fn execute_from(self, runtime: &OrbitRuntime, cwd: &Path) -> CommandOut {
        let global_root = runtime.global_root();
        let registry_path = workspace_registry::registry_path_for(&global_root);
        // Keep target validation, deregistration and local deletion together;
        // another initializer must not register this checkout midway through teardown.
        workspace_registry::with_registry_lock(&registry_path, || {
            let mut registry = workspace_registry::load_registry_from(&registry_path)?;
            let (workspace, checkout) = resolve_teardown_target(&registry, &self.workspace)?;
            refuse_if_cwd_belongs_to_another_checkout(&registry, &checkout, cwd)?;

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

            let partitions = checkout_task_store_partitions(
                &global_root,
                &orbit_dir,
                Some(workspace_id.as_str()),
            )?;
            let plan = format_teardown_plan(&workspace, &checkout, &partitions);
            if !self.confirm {
                return Err(OrbitError::InvalidInput(format!(
                    "teardown is destructive. Resolved target:\n{plan}\nPass --confirm to proceed."
                )));
            }

            let mut removed: Vec<String> = Vec::new();

            // 1. Deregister from workspace registry (before deleting .orbit/)
            let ws = workspace_registry::remove_workspace(&mut registry, &workspace_id)?;
            workspace_registry::save_registry_to(&registry, &registry_path)?;
            removed.push(format!(
                "deregistered workspace '{}' from registry",
                ws.name
            ));

            // 2. Delete the task-store partition this checkout's task state is
            //    bound to. The catalog id above is not that partition's name
            //    unless the two id spaces happen to coincide, so the task registry
            //    resolves it and retires its bindings [ORB-12119].
            for partition in
                remove_checkout_task_stores(&global_root, &orbit_dir, Some(&workspace_id))?
            {
                removed.push(format_deleted_partition(&partition, &workspace_name));
            }

            // 3. Remove the legacy repo-local skill links workspace init wrote
            //    into .agents/skills/ and .claude/skills/. Only links into this
            //    checkout's .orbit/skills/ are Orbit-owned; must run before step 4
            //    deletes that target.
            for dir_name in LEGACY_SKILL_DISCOVERY_DIRS {
                let cleanup = remove_owned_skill_links(&repo_root, &orbit_dir, dir_name)?;
                if cleanup.removed_links > 0 {
                    removed.push(format!(
                        "removed {} Orbit skill link{} from {dir_name}/skills/",
                        cleanup.removed_links,
                        if cleanup.removed_links == 1 { "" } else { "s" }
                    ));
                }
                for dir in &cleanup.removed_dirs {
                    removed.push(format!("removed empty {}", dir.display()));
                }
            }

            // 4. Delete .orbit/ directory
            if orbit_dir.is_dir() {
                std::fs::remove_dir_all(&orbit_dir).map_err(|e| OrbitError::Io(e.to_string()))?;
                removed.push(format!("deleted {}", orbit_dir.display()));
            }

            let mut text = format!("teardown plan:\n{plan}\nteardown complete:");
            for item in &removed {
                text.push_str(&format!("\n  - {item}"));
            }
            if removed.is_empty() {
                text.push_str("\n  (nothing to remove)");
            }

            Ok(Payload::detail(
                json!({
                    "workspace": workspace_name,
                    "id": workspace_id,
                    "checkout": repo_root.display().to_string(),
                    "removed": removed,
                }),
                text,
            )
            .into())
        })
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
    cwd: &Path,
) -> Result<(), OrbitError> {
    let Some(cwd_checkout) = workspace_registry::find_checkout_by_path(registry, cwd) else {
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

fn format_teardown_plan(
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

fn format_deleted_partition(path: &Path, workspace_name: &str) -> String {
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
