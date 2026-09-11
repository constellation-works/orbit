//! Task-store partition paths and ownership, shared by `workspace teardown`
//! and `doctor`'s orphan-partition check/fix [ORB-12109].
//!
//! `orbit-store` owns the per-workspace bundle layout
//! (`<global_root>/tasks/workspaces/<workspace_id>/`) but knows nothing
//! about the workspace registry; this module is the composition seam that
//! lets a caller resolve or remove one workspace's partition without
//! reaching around `orbit-store` from `orbit-cli`.
//!
//! The partition directory name is a *task-registry* workspace id
//! (`workspace_bindings.workspace_id` in `<global_root>/tasks/index.sqlite`),
//! which is minted as `<slug>-<hash>` whenever a checkout binds without an
//! explicit id. It is not the workspace catalog's `ws_*` id space, so the
//! task registry — not `workspaces.json` — decides which partition a checkout's
//! task state lives in and which partitions are still claimed [ORB-12119].

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_core::runtime::UNBOUND_DATA_DIR_WORKSPACE_ID;
use orbit_registry::workspace_registry;
pub use orbit_store::maintenance::task_registry::task_workspaces_dir;
use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};
use orbit_types::task::is_valid_orb_task_id;

/// Path to one workspace's task-store partition under
/// `<global_root>/tasks/workspaces/<workspace_id>/`.
pub fn task_store_partition_path(global_root: &Path, workspace_id: &str) -> PathBuf {
    task_workspaces_dir(global_root).join(workspace_id)
}

/// One partition directory that no registry claims, with the amount of task
/// data it still holds.
#[derive(Debug, Clone)]
pub struct UnclaimedPartition {
    /// The partition directory itself.
    pub path: PathBuf,
    /// Task bundles directly inside it — each a directory named for a task id.
    pub task_bundles: usize,
}

/// What a scan of `<global_root>/tasks/workspaces/` found.
///
/// Unclaimed partitions are split by whether they still hold task bundles,
/// because on disk a partition abandoned by `workspace teardown` and a live
/// partition whose registry row was lost are the same thing. Only the empty
/// ones can be deleted from that evidence alone [ORB-12131].
#[derive(Debug, Clone)]
pub struct TaskStorePartitions {
    /// Partition directories present on this host.
    pub scanned: usize,
    /// Unclaimed and empty of task bundles: nothing to lose by deleting them.
    pub removable: Vec<UnclaimedPartition>,
    /// Unclaimed but still holding task bundles, which `orbit task reindex`
    /// can rebind from the bundles themselves. Never deleted automatically.
    pub unowned: Vec<UnclaimedPartition>,
}

/// Classify every task-store partition on this host as claimed, removable, or
/// unowned-but-populated.
///
/// `None` means the directory has never been created (fresh host, no task ever
/// committed) — nothing to diagnose rather than nothing orphaned.
pub fn inspect_task_store_partitions(
    global_root: &Path,
) -> Result<Option<TaskStorePartitions>, OrbitError> {
    let Some(partitions) = task_store_partitions(global_root)? else {
        return Ok(None);
    };
    let claimed = claimed_partition_ids(global_root)?;
    let scanned = partitions.len();

    let (unowned, removable) = partitions
        .into_iter()
        .filter(|path| !path_is_claimed(path, &claimed))
        .map(|path| UnclaimedPartition {
            task_bundles: count_task_bundles(&path),
            path,
        })
        .partition(|partition| partition.task_bundles > 0);

    Ok(Some(TaskStorePartitions {
        scanned,
        removable,
        unowned,
    }))
}

/// Delete every unclaimed partition that holds no task bundles, retiring any
/// registry rows that name it. Returns the removed partition paths.
///
/// A partition that still holds bundles is left alone however the registry
/// answers: an unclaimed populated partition is exactly the state a lost or
/// rebuilt `tasks/index.sqlite` produces for every checkout other than the one
/// the command runs from, and deleting it would destroy task data that
/// `orbit task reindex` can otherwise recover from the bundles [ORB-12131].
/// Reclaiming a genuinely dead populated partition stays a deliberate manual
/// step, or `orbit workspace teardown` while the checkout still exists.
pub fn remove_unclaimed_task_stores(global_root: &Path) -> Result<Vec<PathBuf>, OrbitError> {
    let Some(partitions) = inspect_task_store_partitions(global_root)? else {
        return Ok(Vec::new());
    };
    let tasks = open_task_registry(global_root)?;

    let mut removed = Vec::new();
    for partition in partitions.removable {
        let Some(workspace_id) = partition_id(&partition.path) else {
            continue;
        };
        if remove_partition(&tasks, global_root, workspace_id)? {
            removed.push(partition.path);
        }
    }
    Ok(removed)
}

/// Delete the task-store partitions a torn-down checkout leaves behind:
/// the partition its task state is actually bound to, plus a partition named
/// for its workspace-catalog id when one exists and no other checkout is bound
/// to it. Returns the removed partition paths.
///
/// The second case only arises for a checkout whose catalog id and bound
/// partition id coincide, or for a partition left over from before the two id
/// spaces were told apart; a partition another checkout still binds is never
/// this checkout's to delete.
pub fn remove_checkout_task_stores(
    global_root: &Path,
    orbit_dir: &Path,
    catalog_workspace_id: Option<&str>,
) -> Result<Vec<PathBuf>, OrbitError> {
    let tasks = open_task_registry(global_root)?;
    let mut targets: Vec<String> = Vec::new();

    if let Some(bound) = tasks.find_checkout_by_orbit_dir(orbit_dir)? {
        targets.push(bound.workspace_id);
    }
    if let Some(catalog_id) = catalog_workspace_id
        && !targets.iter().any(|id| id == catalog_id)
        && !bound_to_another_checkout(&tasks, catalog_id, orbit_dir)?
    {
        targets.push(catalog_id.to_string());
    }

    let mut removed = Vec::new();
    for workspace_id in targets {
        if remove_partition(&tasks, global_root, &workspace_id)? {
            removed.push(task_store_partition_path(global_root, &workspace_id));
        }
    }
    Ok(removed)
}

/// Partition the checkout at `orbit_dir` binds its task state to, when it is
/// bound — the directory a caller must look in to find that checkout's task
/// bundles, whatever the workspace catalog calls the same checkout.
pub fn bound_partition_id(
    global_root: &Path,
    orbit_dir: &Path,
) -> Result<Option<String>, OrbitError> {
    Ok(open_task_registry(global_root)?
        .find_checkout_by_orbit_dir(orbit_dir)?
        .map(|binding| binding.workspace_id))
}

/// Whether the task registry still binds `workspace_id` as a partition.
pub fn partition_is_bound(global_root: &Path, workspace_id: &str) -> Result<bool, OrbitError> {
    Ok(open_task_registry(global_root)?
        .workspace_ids()?
        .contains(workspace_id))
}

/// Every workspace id that still claims a partition on this host: the task
/// registry's own bindings, the workspace catalog's ids, and the synthetic
/// partition every `--root <data-dir>` write lands in, which by construction
/// appears in neither registry.
fn claimed_partition_ids(global_root: &Path) -> Result<BTreeSet<String>, OrbitError> {
    let tasks = open_task_registry(global_root)?;
    let mut claimed = tasks.workspace_ids()?;
    claimed.insert(UNBOUND_DATA_DIR_WORKSPACE_ID.to_string());

    let registry_path = workspace_registry::registry_path_for(global_root);
    if registry_path.exists() {
        let registry = workspace_registry::load_registry_from(&registry_path)?;
        claimed.extend(
            registry
                .workspaces
                .into_iter()
                .map(|workspace| workspace.id),
        );
    }
    Ok(claimed)
}

/// Open the task registry that names the partitions, rather than creating one
/// as a side effect of asking who owns a directory. An absent registry answers
/// "nothing is claimed" for every partition on the host, so report it instead.
///
/// This only catches a caller that reaches the registry before any runtime has
/// built it. Every `orbit` subcommand bootstraps the global root first, which
/// recreates an empty `tasks/index.sqlite`, so a registry lost since the last
/// command is indistinguishable here from a legitimately empty one; refusing
/// to delete populated partitions is what protects that case
/// ([`remove_unclaimed_task_stores`]) [ORB-12131].
fn open_task_registry(global_root: &Path) -> Result<TaskRegistryStore, OrbitError> {
    let path = task_registry_path(global_root);
    if !path.exists() {
        return Err(OrbitError::Io(format!(
            "task registry {} is missing, so no task-store partition's owner can be resolved",
            path.display()
        )));
    }
    TaskRegistryStore::open(&path)
}

/// Immediate subdirectories of `<global_root>/tasks/workspaces/`, one per
/// partition. `None` when the directory has never been created.
fn task_store_partitions(global_root: &Path) -> Result<Option<Vec<PathBuf>>, OrbitError> {
    let workspaces_dir = task_workspaces_dir(global_root);
    let entries = match std::fs::read_dir(&workspaces_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(OrbitError::Io(format!(
                "read {}: {error}",
                workspaces_dir.display()
            )));
        }
    };
    Ok(Some(
        entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect(),
    ))
}

/// Workspace id a partition directory is named for.
pub fn partition_id(partition: &Path) -> Option<&str> {
    partition.file_name().and_then(|name| name.to_str())
}

/// Task bundles directly under one partition: subdirectories named for a task
/// id, counting a `<task-id>.deleted` tombstone as data the way
/// `orbit task reindex` does when it rebuilds the index from these directories.
fn count_task_bundles(partition: &Path) -> usize {
    std::fs::read_dir(partition)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter(|entry| {
            entry.file_name().to_str().is_some_and(|name| {
                is_valid_orb_task_id(name.strip_suffix(".deleted").unwrap_or(name))
            })
        })
        .count()
}

fn path_is_claimed(partition: &Path, claimed: &BTreeSet<String>) -> bool {
    partition_id(partition).is_some_and(|id| claimed.contains(id))
}

/// Whether `workspace_id`'s partition holds *another* checkout's task state,
/// and so is not this checkout's to delete. The registry normalizes the paths
/// it stores, so the comparison canonicalizes too.
fn bound_to_another_checkout(
    tasks: &TaskRegistryStore,
    workspace_id: &str,
    orbit_dir: &Path,
) -> Result<bool, OrbitError> {
    let binding = match tasks.find_workspace_checkout(workspace_id) {
        Ok(binding) => binding,
        // Not a well-formed workspace id, so no binding can name it.
        Err(OrbitError::InvalidInput(_)) => return Ok(false),
        Err(error) => return Err(error),
    };
    let canonical = std::fs::canonicalize(orbit_dir).unwrap_or_else(|_| orbit_dir.to_path_buf());
    Ok(binding.is_some_and(|binding| binding.orbit_dir != canonical))
}

/// Retire one partition's registry bindings, then delete its directory —
/// bindings first, so an interrupted removal leaves recoverable bundles rather
/// than a binding pointing at a directory that is gone. Returns whether a
/// partition directory was deleted; bindings are retired either way, including
/// for a workspace that never wrote a bundle.
fn remove_partition(
    tasks: &TaskRegistryStore,
    global_root: &Path,
    workspace_id: &str,
) -> Result<bool, OrbitError> {
    // A directory name that is not a well-formed workspace id can hold no
    // bindings; its directory is still this function's to remove.
    if let Err(error) = tasks.unbind_workspace(workspace_id)
        && !matches!(error, OrbitError::InvalidInput(_))
    {
        return Err(error);
    }

    let path = task_store_partition_path(global_root, workspace_id);
    if !path.is_dir() {
        return Ok(false);
    }
    std::fs::remove_dir_all(&path).map_err(|error| {
        OrbitError::Io(format!("remove task store {}: {error}", path.display()))
    })?;
    Ok(true)
}
