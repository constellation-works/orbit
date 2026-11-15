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
use std::sync::{Mutex, PoisonError};

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

/// Bootstrap the read-only runtime `orbit task show <id>` must read.
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
        return RegisteredRuntimeFactory::initialize_read_only_with_overrides(
            root_override,
            selector,
        );
    }
    let global_root = global_root_for(root_override)?;
    // `task show` only reads the registry, so look the owner up through a
    // read-only handle. A registry that cannot be opened that way (missing, or
    // needing a writable migration) takes the ordinary writable open, which
    // creates or migrates it exactly as before and reports the real error.
    let selected = match TaskRegistryStore::open_read_only(&task_registry_path(&global_root)) {
        Ok(tasks) => resolve_task_owner_in(&global_root, &tasks, task_id)?,
        Err(_) => resolve_task_owner(&global_root, task_id)?,
    };
    RegisteredRuntimeFactory::open_registered_checkout_read_only(
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
    resolve_task_owner_in(global_root, &tasks, task_id)
}

/// [`resolve_task_owner`] against a registry handle the caller already holds.
///
/// The partition -> checkout join still reads `workspaces.json` and each
/// checkout's `config.yaml` on every call, so a newly registered or removed
/// checkout is seen on the next lookup.
pub fn resolve_task_owner_in(
    global_root: &Path,
    tasks: &TaskRegistryStore,
    task_id: &str,
) -> Result<ResolvedWorkspaceSelection, OrbitError> {
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
        local_root: checkout.orbit_dir.clone(),
    })
}

/// Owner lookups for a long-lived host: one lazily opened registry handle,
/// reused across calls and reopened when the database file is replaced.
///
/// The handle is read-only. Each lookup is one indexed read, and SQLite shows
/// every commit another process makes to the registry on the next statement, so
/// a task bound after the handle opened is still found. A registry that cannot
/// be opened read-only (missing, or needing a writable migration) falls back to
/// [`resolve_task_owner`] for that call and is retried on the next one.
#[derive(Default)]
pub struct TaskOwnerLookup {
    cached: Mutex<Option<CachedRegistry>>,
}

struct CachedRegistry {
    identity: RegistryFileIdentity,
    store: TaskRegistryStore,
}

/// Detects registry replacement without reacting to ordinary writes.
#[derive(Debug, PartialEq, Eq)]
struct RegistryFileIdentity {
    created: Option<std::time::SystemTime>,
    #[cfg(unix)]
    device_inode: (u64, u64),
}

impl RegistryFileIdentity {
    fn read(path: &Path) -> Option<Self> {
        let metadata = std::fs::metadata(path).ok()?;
        Some(Self {
            created: metadata.created().ok(),
            #[cfg(unix)]
            device_inode: {
                use std::os::unix::fs::MetadataExt;
                (metadata.dev(), metadata.ino())
            },
        })
    }
}

impl TaskOwnerLookup {
    /// Resolve the checkout that owns `task_id` with the same results and
    /// errors as [`resolve_task_owner`].
    pub fn resolve(
        &self,
        global_root: &Path,
        task_id: &str,
    ) -> Result<ResolvedWorkspaceSelection, OrbitError> {
        let path = task_registry_path(global_root);
        let store = {
            let mut cached = self.cached.lock().unwrap_or_else(PoisonError::into_inner);
            let identity = RegistryFileIdentity::read(&path);
            if cached
                .as_ref()
                .is_some_and(|held| Some(&held.identity) != identity.as_ref())
            {
                *cached = None;
            }
            if cached.is_none()
                && let Some(identity) = identity
                && let Ok(store) = TaskRegistryStore::open_read_only(&path)
            {
                *cached = Some(CachedRegistry { identity, store });
            }
            cached.as_ref().map(|held| held.store.clone())
        };
        match store {
            Some(store) => resolve_task_owner_in(global_root, &store, task_id),
            None => resolve_task_owner(global_root, task_id),
        }
    }
}

/// Registry identity of the workspace `runtime` is bound to, when its checkout
/// is registered on this machine.
///
/// Reported by `task show` on every path — cwd, `--workspace`, and registry
/// ownership alike — so the output always names where the record was read from.
/// An unregistered checkout (a bare `orbit init` directory) simply has no
/// registry identity to report.
pub fn bound_workspace_identity(runtime: &OrbitRuntime) -> Option<WorkspaceIdentity> {
    // The open already loaded this registry. Use that snapshot, including a
    // miss: re-reading the file would race the open and repeat the load.
    if let Some(opened) = crate::registry_runtime::opened_selector_inputs(runtime) {
        return workspace_registry::find_workspace_by_path(
            &opened.registry,
            &runtime.paths().repo_root,
        )
        .map(WorkspaceIdentity::of);
    }
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
