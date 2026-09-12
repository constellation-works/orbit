//! Which registered workspace owns a globally unique task ID.
//!
//! Task IDs are a machine-global primary key in the coordination task registry,
//! so `task show` follows the ID instead of the caller's cwd or MCP session
//! [ORB-10797]. This module is the single owner of that lookup and of the
//! registry identity (`orbit (ws_orbit)`) the command reports; the CLI
//! subcommand and the authoritative MCP server both call it rather than
//! repeating the join.
//!
//! Scope is deliberately `task show`. Every other verb keeps cwd or the
//! announced session workspace as its default, because only a read addressed
//! by a globally unique ID can be routed from the ID alone.

use std::path::Path;

use orbit_common::{NotFoundKind, OrbitError};
use orbit_core::OrbitRuntime;
use orbit_registry::workspace_registry;
use orbit_store::maintenance::task_registry::{
    TaskRegistryStore, read_workspace_config_optional, task_registry_path,
};
use orbit_types::workspace::{Workspace, WorkspaceStatus};

use crate::registry_runtime::{
    RegisteredRuntimeFactory, ResolvedWorkspaceSelection, global_root_for,
};

/// How `task show` names the workspace a task landed in.
///
/// The logical ID is the half a later write addresses (`orbit --workspace
/// ws_orbit task update ...`); the name is the half a human recognizes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceIdentity {
    pub id: String,
    pub name: String,
}

impl WorkspaceIdentity {
    fn of(workspace: &Workspace) -> Self {
        Self {
            id: workspace.id.clone(),
            name: workspace.name.clone(),
        }
    }

    /// `orbit (ws_orbit)` — the rendering both the human line and the error
    /// messages below use.
    pub fn label(&self) -> String {
        format!("{} ({})", self.name, self.id)
    }
}

/// Bootstrap the runtime `orbit task show <id>` must read.
///
/// With `--workspace`, the selector is a filter: this is the ordinary
/// registered bootstrap, and a task owned elsewhere simply is not found there.
/// Without one, the task registry — not the cwd walk — names the owner, so the
/// command works from a foreign checkout and from a directory that is no
/// workspace at all.
pub fn initialize_for_task_show(
    root_override: Option<&Path>,
    workspace_selector: Option<&str>,
    task_id: &str,
) -> Result<OrbitRuntime, OrbitError> {
    let selector = workspace_selector
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if selector.is_some() {
        return RegisteredRuntimeFactory::initialize_with_overrides(root_override, selector);
    }
    let global_root = global_root_for(root_override)?;
    let selected = resolve_task_owner(&global_root, task_id)?;
    RegisteredRuntimeFactory::open_registered_checkout(
        &global_root,
        &selected.workspace,
        &selected.checkout,
    )
}

/// Resolve the registered checkout that owns `task_id`.
///
/// An ID the registry has never seen is a plain not-found — the registry is the
/// index, and `orbit task reindex` is what repairs a bundle missing from it. An
/// ID it knows whose owning checkout is gone, unreadable, or inactive is *not*
/// a not-found: those name the owning workspace, because "unknown task" would
/// send the caller looking for the wrong problem.
pub fn resolve_task_owner(
    global_root: &Path,
    task_id: &str,
) -> Result<ResolvedWorkspaceSelection, OrbitError> {
    let tasks = TaskRegistryStore::open(&task_registry_path(global_root))?;
    let binding = tasks
        .find_task_binding(task_id)?
        .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, task_id.to_string()))?;
    let partition = binding.partition_id;

    let registry = workspace_registry::load_registry_from(&workspace_registry::registry_path_for(
        global_root,
    ))?;
    let owner = workspace_registry::local_workspaces(&registry).find(|(workspace, checkout)| {
        checkout_partition_id(&checkout.orbit_dir).is_some_and(|id| id == partition)
            || (checkout.orbit_dir == global_root && workspace.id == partition)
    });
    let Some((workspace, checkout)) = owner else {
        // The task registry keeps the partition's slug even when no checkout
        // answers to it, so the caller still learns which workspace to repair.
        let slug = tasks
            .find_workspace_binding(&partition)
            .ok()
            .flatten()
            .map(|binding| binding.slug)
            .unwrap_or_else(|| partition.clone());
        return Err(OrbitError::WorkspaceError(format!(
            "task {task_id} is owned by workspace '{slug}' ({partition}), which has no readable local checkout on this machine; restore or re-register that checkout"
        )));
    };
    if workspace.status != WorkspaceStatus::Active {
        return Err(OrbitError::WorkspaceError(format!(
            "task {task_id} is owned by workspace {}, which is {} on this machine",
            WorkspaceIdentity::of(workspace).label(),
            workspace.status
        )));
    }
    Ok(ResolvedWorkspaceSelection {
        workspace: workspace.clone(),
        checkout: checkout.clone(),
    })
}

/// Registry identity of the workspace `runtime` is bound to, when its checkout
/// is registered on this machine.
///
/// Reported by `task show` on every path — cwd, `--workspace`, and registry
/// ownership alike — so the output always names where the record was read from.
/// An unregistered checkout (a bare `orbit init` directory) simply has no
/// registry identity to report.
pub fn bound_workspace_identity(runtime: &OrbitRuntime) -> Option<WorkspaceIdentity> {
    let registry = workspace_registry::load_registry_from(&workspace_registry::registry_path_for(
        &runtime.global_root(),
    ))
    .ok()?;
    workspace_registry::find_workspace_by_path(&registry, &runtime.paths().repo_root)
        .map(WorkspaceIdentity::of)
}

/// Task-store partition id recorded in `<orbit_dir>/config.yaml`, which is the
/// key the task registry partitions by (L-0098: it is a different namespace
/// from the workspace-registry id and may differ from it). A checkout that has
/// been deleted or never initialized has none, and is simply not a candidate
/// owner.
fn checkout_partition_id(orbit_dir: &Path) -> Option<String> {
    read_workspace_config_optional(orbit_dir)
        .ok()
        .flatten()
        .map(|config| config.workspace_id)
}
