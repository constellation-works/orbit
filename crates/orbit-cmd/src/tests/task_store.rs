//! Sibling tests for `task_store.rs` — which partition a checkout's task state
//! lives in, and what removing one retires [ORB-12119].

use std::fs;
use std::path::Path;

use orbit_store::maintenance::task_registry::{
    BindWorkspaceParams, TaskRegistryStore, task_registry_path, task_workspaces_dir,
};

use crate::task_store::{
    bound_partition_id, partition_is_bound, remove_checkout_task_stores, task_store_partition_path,
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
