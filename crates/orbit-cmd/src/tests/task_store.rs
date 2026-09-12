//! Sibling tests for `task_store.rs` — which partition a checkout's task state
//! lives in, and what removing one retires [ORB-12119].

use std::fs;
use std::path::Path;

use chrono::Utc;
use orbit_registry::workspace_registry;
use orbit_store::maintenance::task_registry::{
    BindWorkspaceParams, RegisterWorkspaceParams, TaskRegistryStore, task_registry_path,
    task_workspaces_dir,
};
use orbit_types::workspace::{Workspace, WorkspaceCheckout, WorkspaceStatus};

use crate::task_store::{
    bound_partition_id, inspect_task_store_partitions, partition_is_bound,
    remove_checkout_task_stores, remove_unclaimed_task_stores, retain_task_store_on_catalog_remove,
    task_store_partition_path,
};

fn bind(global_root: &Path, workspace_id: &str, slug: &str, repo_root: &Path) {
    let tasks =
        TaskRegistryStore::open(&task_registry_path(global_root)).expect("open task registry");
    tasks
        .bind_workspace(BindWorkspaceParams {
            workspace_id: Some(workspace_id.to_string()),
            slug: slug.to_string(),
            repo_root: repo_root.to_path_buf(),
            workspace_path: repo_root.to_path_buf(),
            orbit_dir: repo_root.join(".orbit"),
            repo_fingerprint: None,
        })
        .expect("bind task-registry workspace");
}

fn register_logical_workspace(global_root: &Path, workspace_id: &str, slug: &str) {
    let tasks =
        TaskRegistryStore::open(&task_registry_path(global_root)).expect("open task registry");
    tasks
        .register_workspace(RegisterWorkspaceParams {
            workspace_id: workspace_id.to_string(),
            slug: slug.to_string(),
            repo_fingerprint: None,
        })
        .expect("register logical workspace");
}

/// Delete the task registry the way a corrupted restore or a manual rebuild
/// does, leaving every partition directory in place.
fn lose_task_registry(global_root: &Path) {
    let registry = task_registry_path(global_root);
    let name = registry
        .file_name()
        .expect("task registry path names a file")
        .to_owned();
    for suffix in ["", "-wal", "-shm"] {
        let mut sidecar = name.clone();
        sidecar.push(suffix);
        let path = registry.with_file_name(sidecar);
        if path.exists() {
            fs::remove_file(&path).expect("remove task registry file");
        }
    }
}

fn write_task_bundle(global_root: &Path, workspace_id: &str, task_id: &str) {
    let bundle = task_workspaces_dir(global_root)
        .join(workspace_id)
        .join(task_id);
    fs::create_dir_all(&bundle).expect("create task bundle dir");
    fs::write(bundle.join("task.yaml"), b"id: dummy\n").expect("write bundle file");
}

/// A checkout bound under a derived `<slug>-<hash>` id keeps its task state
/// there, not under the `ws_*` id the workspace catalog knows it by. Removing
/// the checkout's task stores must follow the binding and retire it.
#[test]
fn removing_a_checkout_deletes_the_bound_partition_and_retires_its_bindings() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let repo_root = temp.path().join("repo");
    let orbit_dir = repo_root.join(".orbit");
    fs::create_dir_all(&global_root).expect("create global root");
    fs::create_dir_all(&orbit_dir).expect("create checkout");

    bind(&global_root, "repo-a1b2c3", "repo", &repo_root);
    write_task_bundle(&global_root, "repo-a1b2c3", "ORB-1");
    // An unrelated checkout's partition, which this removal must not touch.
    let other_root = temp.path().join("other");
    fs::create_dir_all(other_root.join(".orbit")).expect("create other checkout");
    bind(&global_root, "other-d4e5f6", "other", &other_root);
    write_task_bundle(&global_root, "other-d4e5f6", "ORB-2");

    assert_eq!(
        bound_partition_id(&global_root, &orbit_dir).expect("resolve bound partition"),
        Some("repo-a1b2c3".to_string())
    );

    let removed = remove_checkout_task_stores(&global_root, &orbit_dir, Some("ws_repo"))
        .expect("remove checkout task stores");
    assert_eq!(
        removed,
        vec![task_store_partition_path(&global_root, "repo-a1b2c3")]
    );
    assert!(
        !task_workspaces_dir(&global_root)
            .join("repo-a1b2c3")
            .exists()
    );
    assert!(
        !partition_is_bound(&global_root, "repo-a1b2c3").expect("read bindings"),
        "no binding may survive pointing at the deleted bundle directory"
    );

    assert!(
        task_workspaces_dir(&global_root)
            .join("other-d4e5f6")
            .is_dir(),
        "an unrelated checkout's partition must survive"
    );
    assert!(partition_is_bound(&global_root, "other-d4e5f6").expect("read bindings"));
}

/// A partition named for the removed checkout's catalog id, but bound to a
/// different checkout, holds that checkout's live tasks.
#[test]
fn removing_a_checkout_leaves_a_catalog_named_partition_another_checkout_binds() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let repo_root = temp.path().join("repo");
    let orbit_dir = repo_root.join(".orbit");
    let other_root = temp.path().join("other");
    fs::create_dir_all(&global_root).expect("create global root");
    fs::create_dir_all(&orbit_dir).expect("create checkout");
    fs::create_dir_all(other_root.join(".orbit")).expect("create other checkout");

    bind(&global_root, "ws_shared", "other", &other_root);
    write_task_bundle(&global_root, "ws_shared", "ORB-1");

    let removed = remove_checkout_task_stores(&global_root, &orbit_dir, Some("ws_shared"))
        .expect("remove checkout task stores");
    assert!(removed.is_empty(), "nothing of this checkout's to remove");
    assert!(task_workspaces_dir(&global_root).join("ws_shared").is_dir());
    assert!(partition_is_bound(&global_root, "ws_shared").expect("read bindings"));
}

/// A checkout with no task-registry binding still leaves a partition named for
/// its catalog id, as an older binary wrote it.
#[test]
fn removing_a_checkout_deletes_an_unbound_catalog_named_partition() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let orbit_dir = temp.path().join("repo").join(".orbit");
    fs::create_dir_all(&global_root).expect("create global root");
    fs::create_dir_all(&orbit_dir).expect("create checkout");

    // Open the registry once so the fixture has one, as any real host does.
    TaskRegistryStore::open(&task_registry_path(&global_root)).expect("open task registry");
    write_task_bundle(&global_root, "ws_legacy", "ORB-1");

    let removed = remove_checkout_task_stores(&global_root, &orbit_dir, Some("ws_legacy"))
        .expect("remove checkout task stores");
    assert_eq!(
        removed,
        vec![task_store_partition_path(&global_root, "ws_legacy")]
    );
    assert!(!task_workspaces_dir(&global_root).join("ws_legacy").exists());
}

/// [ORB-12131] A lost `tasks/index.sqlite` is recreated empty by the next
/// runtime bootstrap, which rebinds only the checkout the command runs from.
/// Every other checkout's live partition then looks unclaimed, so the repair
/// must leave those bundles for `orbit task reindex` instead of deleting them.
#[test]
fn losing_the_task_registry_leaves_populated_partitions_for_reindex() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let here = temp.path().join("here");
    let elsewhere = temp.path().join("elsewhere");
    fs::create_dir_all(&global_root).expect("create global root");
    fs::create_dir_all(here.join(".orbit")).expect("create local checkout");
    fs::create_dir_all(elsewhere.join(".orbit")).expect("create remote checkout");

    bind(&global_root, "here-a1b2c3", "here", &here);
    write_task_bundle(&global_root, "here-a1b2c3", "ORB-1");
    bind(&global_root, "elsewhere-d4e5f6", "elsewhere", &elsewhere);
    write_task_bundle(&global_root, "elsewhere-d4e5f6", "ORB-2");

    lose_task_registry(&global_root);
    // The bootstrap that recreates the registry rebinds only this checkout.
    bind(&global_root, "here-a1b2c3", "here", &here);

    let partitions = inspect_task_store_partitions(&global_root)
        .expect("inspect partitions")
        .expect("partitions directory exists");
    assert_eq!(partitions.scanned, 2);
    assert!(
        partitions.removable.is_empty(),
        "no populated partition may be offered for deletion: {:?}",
        partitions.removable
    );
    assert_eq!(
        partitions
            .unowned
            .iter()
            .map(|partition| partition.path.clone())
            .collect::<Vec<_>>(),
        vec![task_store_partition_path(&global_root, "elsewhere-d4e5f6")]
    );

    let removed = remove_unclaimed_task_stores(&global_root).expect("run the repair");
    assert!(removed.is_empty(), "the repair removed {removed:?}");
    assert!(
        task_workspaces_dir(&global_root)
            .join("elsewhere-d4e5f6")
            .join("ORB-2")
            .is_dir(),
        "the unclaimed checkout's task bundle must survive the repair"
    );
}

/// An imported workspace is registered on the host without a machine-local
/// checkout. Its partition is still owned task state and must survive the
/// orphan-store scan and repair.
#[test]
fn registered_checkoutless_workspace_claims_its_partition() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    fs::create_dir_all(&global_root).expect("create global root");

    register_logical_workspace(&global_root, "ws_mirror", "mirror");
    write_task_bundle(&global_root, "ws_mirror", "ORB-1");

    let partitions = inspect_task_store_partitions(&global_root)
        .expect("inspect partitions")
        .expect("partitions directory exists");
    assert_eq!(partitions.scanned, 1);
    assert!(partitions.removable.is_empty(), "{partitions:?}");
    assert!(partitions.stale.is_empty(), "{partitions:?}");
    assert!(partitions.unowned.is_empty(), "{partitions:?}");
    assert!(partitions.unreachable.is_empty(), "{partitions:?}");

    let removed = remove_unclaimed_task_stores(&global_root).expect("run the repair");
    assert!(removed.is_empty(), "the repair removed {removed:?}");
    assert!(
        task_workspaces_dir(&global_root)
            .join("ws_mirror")
            .join("ORB-1")
            .is_dir(),
        "a registered checkout-less workspace's task bundle must survive"
    );
    assert!(
        partition_is_bound(&global_root, "ws_mirror").expect("read bindings"),
        "the logical workspace registration must survive"
    );
}

fn write_catalog_workspace(global_root: &Path, workspace_id: &str, name: &str) {
    let registry_path = workspace_registry::registry_path_for(global_root);
    let mut registry =
        workspace_registry::load_registry_from(&registry_path).expect("load catalog");
    let now = Utc::now();
    workspace_registry::register_workspace(
        &mut registry,
        Workspace {
            id: workspace_id.to_string(),
            name: name.to_string(),
            owner_machine_id: None,
            git_remote: None,
            ship_mode: None,
            base_branch: "main".to_string(),
            status: WorkspaceStatus::Active,
            created_at: now,
            updated_at: now,
        },
    )
    .expect("register catalog workspace");
    workspace_registry::save_registry_to(&registry, &registry_path).expect("save catalog");
}

fn write_catalog_shared_root_checkout(global_root: &Path, workspace_id: &str, repo_root: &Path) {
    let registry_path = workspace_registry::registry_path_for(global_root);
    let mut registry =
        workspace_registry::load_registry_from(&registry_path).expect("load catalog");
    workspace_registry::register_checkout(
        &mut registry,
        WorkspaceCheckout::owner(
            workspace_id.to_string(),
            repo_root.to_path_buf(),
            global_root.to_path_buf(),
        ),
    )
    .expect("register shared-root catalog checkout");
    workspace_registry::save_registry_to(&registry, &registry_path).expect("save catalog");
}

fn drop_catalog_workspace(global_root: &Path, workspace_id: &str) {
    let registry_path = workspace_registry::registry_path_for(global_root);
    let mut registry =
        workspace_registry::load_registry_from(&registry_path).expect("load catalog");
    workspace_registry::remove_workspace(&mut registry, workspace_id).expect("drop catalog");
    workspace_registry::save_registry_to(&registry, &registry_path).expect("save catalog");
}

/// [ORB-12223] Dropping the catalog without copying checkout evidence lets a
/// path-free task-registry binding re-claim the leftover. Retaining evidence
/// first keeps it stale so the repair can reclaim it, even when a survivor
/// already occupies the shared `orbit_dir`.
#[test]
fn retaining_checkout_evidence_keeps_a_shared_root_leftover_stale() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let survivor = temp.path().join("survivor");
    let deleted = temp.path().join("deleted");
    fs::create_dir_all(&global_root).expect("create global root");
    fs::create_dir_all(&survivor).expect("create survivor checkout");
    fs::create_dir_all(&deleted).expect("create deleted checkout");

    bind_at(
        &global_root,
        "ws_survivor",
        "survivor",
        &survivor,
        &global_root,
    );
    write_task_bundle(&global_root, "ws_survivor", "ORB-1");

    register_logical_workspace(&global_root, "ws_deleted", "deleted");
    write_catalog_workspace(&global_root, "ws_deleted", "deleted");
    write_catalog_shared_root_checkout(&global_root, "ws_deleted", &deleted);
    write_task_bundle(&global_root, "ws_deleted", "ORB-2");
    fs::remove_dir_all(&deleted).expect("delete checkout");

    let partitions = inspect_task_store_partitions(&global_root)
        .expect("inspect before catalog drop")
        .expect("partitions directory exists");
    assert_eq!(
        partitions
            .stale
            .iter()
            .map(|partition| partition.path.clone())
            .collect::<Vec<_>>(),
        vec![task_store_partition_path(&global_root, "ws_deleted")]
    );

    let checkout = WorkspaceCheckout::owner(
        "ws_deleted".to_string(),
        deleted.clone(),
        global_root.clone(),
    );
    let leftover =
        retain_task_store_on_catalog_remove(&global_root, "ws_deleted", "deleted", Some(&checkout))
            .expect("retain leftover");
    assert_eq!(leftover.expect("leftover partition").task_bundles, 1);
    drop_catalog_workspace(&global_root, "ws_deleted");

    let partitions = inspect_task_store_partitions(&global_root)
        .expect("inspect after catalog drop")
        .expect("partitions directory exists");
    assert_eq!(
        partitions
            .stale
            .iter()
            .map(|partition| partition.path.clone())
            .collect::<Vec<_>>(),
        vec![task_store_partition_path(&global_root, "ws_deleted")],
        "catalog removal must not re-claim the leftover: {partitions:?}"
    );
    assert!(partitions.unowned.is_empty(), "{partitions:?}");

    let removed = remove_unclaimed_task_stores(&global_root).expect("run the repair");
    assert_eq!(removed.task_bundles_removed(), 1, "{removed:?}");
    assert!(
        !task_workspaces_dir(&global_root)
            .join("ws_deleted")
            .exists()
    );
    assert!(
        task_workspaces_dir(&global_root)
            .join("ws_survivor")
            .join("ORB-1")
            .is_dir()
    );
    assert!(partition_is_bound(&global_root, "ws_survivor").expect("read survivor binding"));
}

fn bind_at(global_root: &Path, workspace_id: &str, slug: &str, repo_root: &Path, orbit_dir: &Path) {
    let tasks =
        TaskRegistryStore::open(&task_registry_path(global_root)).expect("open task registry");
    tasks
        .bind_workspace(BindWorkspaceParams {
            workspace_id: Some(workspace_id.to_string()),
            slug: slug.to_string(),
            repo_root: repo_root.to_path_buf(),
            workspace_path: repo_root.to_path_buf(),
            orbit_dir: orbit_dir.to_path_buf(),
            repo_fingerprint: None,
        })
        .expect("bind task-registry workspace");
}

/// Residue with no task bundles carries nothing `orbit task reindex` could
/// restore, so the repair still reclaims it [ORB-12109].
#[test]
fn an_unclaimed_partition_without_bundles_is_still_reclaimed() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let repo_root = temp.path().join("repo");
    fs::create_dir_all(&global_root).expect("create global root");
    fs::create_dir_all(repo_root.join(".orbit")).expect("create checkout");

    bind(&global_root, "repo-a1b2c3", "repo", &repo_root);
    write_task_bundle(&global_root, "repo-a1b2c3", "ORB-1");
    // Teardown on an older binary retired the bindings and removed the bundles
    // but left the directory behind, along with a stray lock file.
    let residue = task_store_partition_path(&global_root, "gone-f6e5d4");
    fs::create_dir_all(&residue).expect("create residue partition");
    fs::write(residue.join("bundle.lock"), b"").expect("write residue lock file");

    let partitions = inspect_task_store_partitions(&global_root)
        .expect("inspect partitions")
        .expect("partitions directory exists");
    assert!(partitions.unowned.is_empty(), "{:?}", partitions.unowned);

    let removed = remove_unclaimed_task_stores(&global_root).expect("run the repair");
    assert_eq!(removed.empty, vec![residue.clone()]);
    assert!(removed.stale.is_empty(), "{removed:?}");
    assert!(!residue.exists());
    assert!(
        task_workspaces_dir(&global_root)
            .join("repo-a1b2c3")
            .is_dir(),
        "the bound checkout's partition must survive"
    );
}

/// Make `path` unsearchable and report whether the process is actually kept
/// out. A test running as root reads a `0o000` directory regardless, and then
/// has nothing to assert about a permission failure, so its caller skips.
#[cfg(unix)]
fn make_unsearchable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o000)).expect("drop directory modes");
    fs::read_dir(path).is_err()
}

#[cfg(unix)]
fn restore_search(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("restore directory modes");
}

/// [ORB-12143] A checkout behind an unsearchable parent directory — the shape
/// an unmounted volume or a revoked mount permission takes — cannot be stat-ed,
/// which `Path::exists()` reported as a deleted checkout. The repair then
/// deleted the partition's task bundles, the only copy of that task data.
#[cfg(unix)]
#[test]
fn an_unsearchable_parent_keeps_a_populated_partition() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let volume = temp.path().join("volume");
    let repo_root = volume.join("proj");
    fs::create_dir_all(&global_root).expect("create global root");
    fs::create_dir_all(repo_root.join(".orbit")).expect("create checkout");

    bind(&global_root, "proj-5b631f", "proj", &repo_root);
    write_task_bundle(&global_root, "proj-5b631f", "ORB-1");

    // Nothing to assert when the process searches the directory anyway.
    if !make_unsearchable(&volume) {
        restore_search(&volume);
        return;
    }
    let partitions = inspect_task_store_partitions(&global_root).map(|scan| {
        let scan = scan.expect("partitions directory exists");
        (
            scan.stale.len(),
            scan.removable.len(),
            scan.unreachable
                .iter()
                .map(|entry| (entry.partition.path.clone(), entry.reason.clone()))
                .collect::<Vec<_>>(),
        )
    });
    let removed = remove_unclaimed_task_stores(&global_root);
    restore_search(&volume);

    let (stale, removable, unreachable) = partitions.expect("inspect partitions");
    assert_eq!(stale, 0, "an unreadable checkout is not a deleted checkout");
    assert_eq!(removable, 0);
    let (path, reason) = unreachable
        .first()
        .expect("the partition is reported as unreachable");
    assert_eq!(
        path,
        &task_store_partition_path(&global_root, "proj-5b631f")
    );
    assert!(
        reason.contains(&repo_root.join(".orbit").display().to_string())
            || reason.contains(&repo_root.display().to_string()),
        "the reason names the path that could not be resolved: {reason}"
    );

    assert!(
        removed.expect("run the repair").is_empty(),
        "no partition may be deleted while its checkout cannot be stat-ed"
    );
    assert!(
        task_workspaces_dir(&global_root)
            .join("proj-5b631f")
            .join("ORB-1")
            .is_dir(),
        "the task bundle must survive the repair"
    );
    assert!(
        partition_is_bound(&global_root, "proj-5b631f").expect("read bindings"),
        "the binding of a reachable-again checkout must survive too"
    );
}

/// [ORB-12143] Absence is the only stat answer that authorizes deleting
/// bundles. Here the checkout path resolves through a non-directory (`ENOTDIR`),
/// which is a failure to answer rather than a missing checkout.
#[test]
fn a_stat_failure_other_than_absence_keeps_a_populated_partition() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let repo_root = temp.path().join("proj");
    fs::create_dir_all(&global_root).expect("create global root");
    fs::create_dir_all(repo_root.join(".orbit")).expect("create checkout");

    bind(&global_root, "proj-5b631f", "proj", &repo_root);
    write_task_bundle(&global_root, "proj-5b631f", "ORB-1");

    fs::remove_dir_all(&repo_root).expect("remove checkout directory");
    fs::write(&repo_root, b"not a directory").expect("write a file where the checkout was");

    let partitions = inspect_task_store_partitions(&global_root)
        .expect("inspect partitions")
        .expect("partitions directory exists");
    assert!(partitions.stale.is_empty(), "{:?}", partitions.stale);
    assert_eq!(partitions.unreachable.len(), 1, "{partitions:?}");

    let removed = remove_unclaimed_task_stores(&global_root).expect("run the repair");
    assert!(removed.is_empty(), "the repair removed {removed:?}");
    assert!(
        task_workspaces_dir(&global_root)
            .join("proj-5b631f")
            .join("ORB-1")
            .is_dir()
    );
}

/// [ORB-12143] A deleted checkout is still confirmed absent — the enclosing
/// directory is readable and answers that the path is gone — so its partition
/// and bundles are still reclaimed.
#[test]
fn a_confirmed_deleted_checkout_still_releases_its_populated_partition() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let repo_root = temp.path().join("volume").join("proj");
    fs::create_dir_all(&global_root).expect("create global root");
    fs::create_dir_all(repo_root.join(".orbit")).expect("create checkout");

    bind(&global_root, "proj-5b631f", "proj", &repo_root);
    write_task_bundle(&global_root, "proj-5b631f", "ORB-1");
    fs::remove_dir_all(&repo_root).expect("delete checkout");

    let partitions = inspect_task_store_partitions(&global_root)
        .expect("inspect partitions")
        .expect("partitions directory exists");
    assert_eq!(
        partitions
            .stale
            .iter()
            .map(|partition| partition.path.clone())
            .collect::<Vec<_>>(),
        vec![task_store_partition_path(&global_root, "proj-5b631f")]
    );
    assert!(partitions.unreachable.is_empty(), "{partitions:?}");

    let removed = remove_unclaimed_task_stores(&global_root).expect("run the repair");
    assert!(removed.empty.is_empty(), "{removed:?}");
    assert_eq!(
        removed
            .stale
            .iter()
            .map(|partition| partition.path.clone())
            .collect::<Vec<_>>(),
        vec![task_store_partition_path(&global_root, "proj-5b631f")]
    );
    assert_eq!(removed.task_bundles_removed(), 1);
    assert!(!partition_is_bound(&global_root, "proj-5b631f").expect("read bindings"));
}

/// [ORB-12143] An unreachable checkout protects task bundles, not an empty
/// partition directory: with no bundles to lose, a binding that is not a live
/// claim still reclaims the directory.
#[cfg(unix)]
#[test]
fn an_unreachable_checkout_still_reclaims_an_empty_partition() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let volume = temp.path().join("volume");
    let repo_root = volume.join("proj");
    fs::create_dir_all(&global_root).expect("create global root");
    fs::create_dir_all(repo_root.join(".orbit")).expect("create checkout");

    bind(&global_root, "proj-5b631f", "proj", &repo_root);
    let partition = task_store_partition_path(&global_root, "proj-5b631f");
    fs::create_dir_all(&partition).expect("create empty partition dir");

    // Nothing to assert when the process searches the directory anyway.
    if !make_unsearchable(&volume) {
        restore_search(&volume);
        return;
    }
    let removed = remove_unclaimed_task_stores(&global_root);
    restore_search(&volume);

    let removed = removed.expect("run the repair");
    assert_eq!(removed.empty, vec![partition.clone()]);
    assert!(removed.stale.is_empty(), "{removed:?}");
    assert!(!partition.exists());
}
