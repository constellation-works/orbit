//! Sibling tests for `task/store.rs` — which partition a checkout's task state
//! lives in, and what removing one retires [ORB-12119].

use std::fs;
use std::path::Path;

use orbit_store::maintenance::task_registry::{
    BindWorkspaceParams, TaskRegistryStore, task_registry_path, task_workspaces_dir,
};

use crate::task_store::{
    inspect_task_store_partitions, remove_unclaimed_task_stores, task_store_partition_path,
};

fn bind(global_root: &Path, workspace_id: &str, slug: &str, repo_root: &Path) {
    let tasks =
        TaskRegistryStore::open(&task_registry_path(global_root)).expect("open task registry");
    tasks
        .bind_workspace(BindWorkspaceParams {
            partition_id: Some(workspace_id.to_string()),
            slug: slug.to_string(),
            repo_root: repo_root.to_path_buf(),
            workspace_path: repo_root.to_path_buf(),
            orbit_dir: repo_root.join(".orbit"),
            repo_fingerprint: None,
        })
        .expect("bind task-registry workspace");
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

/// [ORB-12131] A lost `tasks/index.sqlite` is recreated empty by the next
/// runtime bootstrap, which rebinds only the checkout the command runs from.
/// Every other checkout's live partition then looks unclaimed, so the repair
/// must leave those bundles for `orbit task reindex` instead of deleting them.
#[test]
fn losing_the_task_registry_leaves_populated_partitions_for_reindex() {
    if crate::tests::run_isolated_test(std::any::type_name_of_val(
        &losing_the_task_registry_leaves_populated_partitions_for_reindex,
    )) {
        return;
    }

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

/// [ORB-12143] Absence is the only stat answer that authorizes deleting
/// bundles. Here the checkout path resolves through a non-directory (`ENOTDIR`),
/// which is a failure to answer rather than a missing checkout.
#[test]
fn a_stat_failure_other_than_absence_keeps_a_populated_partition() {
    if crate::tests::run_isolated_test(std::any::type_name_of_val(
        &a_stat_failure_other_than_absence_keeps_a_populated_partition,
    )) {
        return;
    }

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
