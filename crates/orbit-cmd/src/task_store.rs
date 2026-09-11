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
//! explicit id. `orbit workspace init` may instead bind the catalog's `ws_*`
//! id directly. The task registry and the workspace catalog therefore both
//! contribute claims, using checkout evidence to distinguish live, stale, and
//! unreachable state [ORB-12119].

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_core::runtime::UNBOUND_DATA_DIR_WORKSPACE_ID;
use orbit_registry::workspace_registry;
pub use orbit_store::maintenance::task_registry::task_workspaces_dir;
use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};
use orbit_types::task::is_valid_orb_task_id;
use orbit_types::workspace::WorkspaceCheckout;

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

/// A populated partition whose bound checkout the filesystem could not answer
/// for — neither present nor confirmed absent.
#[derive(Debug, Clone)]
pub struct UnreachablePartition {
    /// The partition and the task data deleting it would cost.
    pub partition: UnclaimedPartition,
    /// The path that could not be resolved and what the filesystem said.
    pub reason: String,
}

/// What a scan of `<global_root>/tasks/workspaces/` found.
///
/// Unclaimed partitions are split into confirmed-dead checkout bindings, empty
/// residue, populated partitions with no binding, and populated partitions
/// whose checkout could not be reached. Deleting task bundles requires
/// evidence a transient filesystem condition cannot forge: a confirmed-absent
/// checkout [ORB-12143]. Anything else populated stays recoverable with
/// `orbit task reindex` [ORB-12131].
#[derive(Debug, Clone)]
pub struct TaskStorePartitions {
    /// Partition directories present on this host.
    pub scanned: usize,
    /// Unclaimed and empty of task bundles: nothing to lose by deleting them.
    pub removable: Vec<UnclaimedPartition>,
    /// Partitions whose task-registry checkout binding points at an orbit
    /// directory confirmed absent. That the checkout is gone is sufficient
    /// evidence to remove the partition, including its bundles.
    pub stale: Vec<UnclaimedPartition>,
    /// Unclaimed but still holding task bundles, which `orbit task reindex`
    /// can rebind from the bundles themselves. Never deleted automatically.
    pub unowned: Vec<UnclaimedPartition>,
    /// Populated partitions whose bound checkout could not be stat-ed — an
    /// unmounted volume, an unsearchable parent, an offline share. Never
    /// deleted automatically: the checkout may be intact behind the failure.
    pub unreachable: Vec<UnreachablePartition>,
}

/// Classify every task-store partition on this host as claimed, removable,
/// confirmed stale, unowned-but-populated, or unreachable.
///
/// `None` means the directory has never been created (fresh host, no task ever
/// committed) — nothing to diagnose rather than nothing orphaned.
pub fn inspect_task_store_partitions(
    global_root: &Path,
) -> Result<Option<TaskStorePartitions>, OrbitError> {
    let Some(partitions) = task_store_partitions(global_root)? else {
        return Ok(None);
    };
    let claims = partition_claims(global_root)?;
    let scanned = partitions.len();

    let mut removable = Vec::new();
    let mut stale = Vec::new();
    let mut unowned = Vec::new();
    let mut unreachable = Vec::new();
    for path in partitions {
        let Some(id) = partition_id(&path).map(str::to_owned) else {
            continue;
        };
        if claims.claimed.contains(&id) {
            continue;
        }

        let partition = UnclaimedPartition {
            task_bundles: count_task_bundles(&path),
            path,
        };
        // An empty partition costs nothing to delete, so any binding that is
        // not a live claim reclaims it; only bundles need stronger evidence.
        if partition.task_bundles == 0 {
            removable.push(partition);
        } else if claims.gone.contains(&id) {
            stale.push(partition);
        } else if let Some(reason) = claims.unreachable.get(&id) {
            unreachable.push(UnreachablePartition {
                partition,
                reason: reason.clone(),
            });
        } else {
            unowned.push(partition);
        }
    }

    Ok(Some(TaskStorePartitions {
        scanned,
        removable,
        stale,
        unowned,
        unreachable,
    }))
}

/// Partitions actually deleted by [`remove_unclaimed_task_stores`], split by
/// whether removing them cost any task data — so a caller can report each
/// count honestly instead of collapsing both into one "empty" figure
/// [ORB-12144].
#[derive(Debug, Clone, Default)]
pub struct RemovedTaskStores {
    /// Empty partitions removed — deleting these cost no task data.
    pub empty: Vec<PathBuf>,
    /// Populated partitions removed because their bound checkout was
    /// confirmed gone, each with the task bundles it held.
    pub stale: Vec<UnclaimedPartition>,
}

impl RemovedTaskStores {
    /// Whether the repair removed no partitions at all.
    pub fn is_empty(&self) -> bool {
        self.empty.is_empty() && self.stale.is_empty()
    }

    /// Task bundles destroyed by removing [`Self::stale`] partitions.
    pub fn task_bundles_removed(&self) -> usize {
        self.stale
            .iter()
            .map(|partition| partition.task_bundles)
            .sum()
    }
}

/// Delete every empty unclaimed partition and every partition whose bound
/// checkout is confirmed gone, retiring any registry rows that name it.
///
/// A partition that still holds bundles is deleted only on evidence a
/// transient filesystem condition cannot forge. An unclaimed populated
/// partition is exactly the state a lost or rebuilt `tasks/index.sqlite`
/// produces for every checkout other than the one the command runs from, and
/// deleting it would destroy task data that `orbit task reindex` can otherwise
/// recover from the bundles [ORB-12131]. A populated partition whose checkout
/// merely failed to stat — an unmounted volume, an unsearchable parent — is
/// kept for the same reason: the checkout may still be there [ORB-12143].
/// Reclaiming such a partition stays a deliberate manual step, or
/// `orbit workspace teardown` while the checkout still exists.
pub fn remove_unclaimed_task_stores(global_root: &Path) -> Result<RemovedTaskStores, OrbitError> {
    let Some(partitions) = inspect_task_store_partitions(global_root)? else {
        return Ok(RemovedTaskStores::default());
    };
    let tasks = open_task_registry(global_root)?;

    let mut removed = RemovedTaskStores::default();
    for partition in partitions.stale {
        let Some(workspace_id) = partition_id(&partition.path) else {
            continue;
        };
        if remove_partition(&tasks, global_root, workspace_id)? {
            removed.stale.push(partition);
        }
    }
    for partition in partitions.removable {
        let Some(workspace_id) = partition_id(&partition.path) else {
            continue;
        };
        if remove_partition(&tasks, global_root, workspace_id)? {
            removed.empty.push(partition.path);
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

/// Who claims each partition on this host, and why the rest do not.
struct PartitionClaims {
    /// Registered task workspaces, live workspace catalog ids, and the
    /// synthetic partition every `--root <data-dir>` write lands in.
    claimed: BTreeSet<String>,
    /// Bindings or catalog checkouts that are confirmed absent.
    gone: BTreeSet<String>,
    /// Bindings or catalog checkouts whose filesystem state could not be
    /// answered, mapped to the failure that stopped the answer.
    unreachable: BTreeMap<String, String>,
}

/// Resolve every partition claim, classifying each task-registry binding by
/// what the filesystem says about the checkout it names.
fn partition_claims(global_root: &Path) -> Result<PartitionClaims, OrbitError> {
    let tasks = open_task_registry(global_root)?;
    let mut claims = PartitionClaims {
        claimed: BTreeSet::new(),
        gone: BTreeSet::new(),
        unreachable: BTreeMap::new(),
    };
    for workspace_id in tasks.workspace_ids()? {
        let Some(checkout) = tasks.find_workspace_checkout(&workspace_id)? else {
            // Imported task archives register their source workspace without a
            // machine-local checkout. That logical registration still owns its
            // partition on this host.
            claims.claimed.insert(workspace_id);
            continue;
        };
        match checkout_evidence(&checkout.orbit_dir) {
            CheckoutEvidence::Present => {
                claims.claimed.insert(workspace_id);
            }
            CheckoutEvidence::Gone => {
                claims.gone.insert(workspace_id);
            }
            CheckoutEvidence::Unreachable(reason) => {
                claims.unreachable.insert(workspace_id, reason);
            }
        }
    }
    claims
        .claimed
        .insert(UNBOUND_DATA_DIR_WORKSPACE_ID.to_string());

    let registry_path = workspace_registry::registry_path_for(global_root);
    if registry_path.exists() {
        let registry = workspace_registry::load_registry_from(&registry_path)?;
        for workspace in registry.workspaces {
            let Some(checkout) = registry
                .checkouts
                .iter()
                .find(|checkout| checkout.workspace_id == workspace.id)
            else {
                // A checkoutless catalog entry is an imported logical
                // workspace. Its partition remains recoverable task state.
                claims.claimed.insert(workspace.id);
                continue;
            };

            // `workspace init` uses the catalog id as its task partition id.
            // Do not let that catalog claim mask stale or unreachable checkout
            // evidence when both id spaces name the same partition.
            record_catalog_checkout_evidence(&mut claims, &workspace.id, checkout);
        }
    }
    Ok(claims)
}

/// Add a catalog checkout's claim without overriding stronger evidence from a
/// task-registry binding that uses the same partition id.
fn record_catalog_checkout_evidence(
    claims: &mut PartitionClaims,
    workspace_id: &str,
    checkout: &WorkspaceCheckout,
) {
    match checkout_evidence(&checkout.orbit_dir) {
        CheckoutEvidence::Present => {
            claims.claimed.insert(workspace_id.to_string());
            claims.gone.remove(workspace_id);
            claims.unreachable.remove(workspace_id);
        }
        CheckoutEvidence::Gone => {
            if !claims.claimed.contains(workspace_id) {
                claims.gone.insert(workspace_id.to_string());
                claims.unreachable.remove(workspace_id);
            }
        }
        CheckoutEvidence::Unreachable(reason) => {
            if !claims.claimed.contains(workspace_id) && !claims.gone.contains(workspace_id) {
                claims.unreachable.insert(workspace_id.to_string(), reason);
            }
        }
    }
}

/// What the filesystem can testify about a bound checkout directory.
enum CheckoutEvidence {
    /// The bound `orbit_dir` is there: the binding is a live claim.
    Present,
    /// The bound `orbit_dir` is absent, and a directory we could actually read
    /// said so: the checkout is gone.
    Gone,
    /// Neither answer was available, naming the path and the failure.
    Unreachable(String),
}

/// Classify one bound checkout directory.
///
/// `Path::exists()` collapses every stat failure into "absent", which made an
/// unmounted volume or an unsearchable parent directory indistinguishable from
/// a deleted checkout — and the repair deletes task bundles on that evidence
/// [ORB-12143]. Absence must therefore be positively confirmed rather than
/// inferred from a failed stat.
fn checkout_evidence(orbit_dir: &Path) -> CheckoutEvidence {
    match std::fs::symlink_metadata(orbit_dir) {
        Ok(_) => CheckoutEvidence::Present,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => confirm_absence(orbit_dir),
        Err(error) => CheckoutEvidence::Unreachable(filesystem_failure(orbit_dir, &error)),
    }
}

/// Confirm that an unstat-able `orbit_dir` is genuinely missing by walking up
/// to the nearest ancestor that exists and listing it. Only a directory we can
/// read can testify that the path beneath it is absent; a stat failure other
/// than `NotFound` anywhere up the chain — `EACCES` from an unsearchable
/// parent, `EIO`/`ENOTCONN` from a dropped mount — is not absence.
fn confirm_absence(orbit_dir: &Path) -> CheckoutEvidence {
    for ancestor in orbit_dir.ancestors().skip(1) {
        match std::fs::symlink_metadata(ancestor) {
            Ok(_) => {
                return match std::fs::read_dir(ancestor) {
                    Ok(_) => CheckoutEvidence::Gone,
                    Err(error) => {
                        CheckoutEvidence::Unreachable(filesystem_failure(ancestor, &error))
                    }
                };
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return CheckoutEvidence::Unreachable(filesystem_failure(ancestor, &error));
            }
        }
    }
    CheckoutEvidence::Unreachable(format!(
        "{}: no readable ancestor directory could confirm it is absent",
        orbit_dir.display()
    ))
}

fn filesystem_failure(path: &Path, error: &std::io::Error) -> String {
    format!("{}: {error}", path.display())
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
