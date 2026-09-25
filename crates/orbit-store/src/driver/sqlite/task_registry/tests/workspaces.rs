//! Workspace and checkout bindings: bind, rebind, fingerprints, listing and
//! retirement.

use std::fs;
use std::path::PathBuf;

use orbit_common::OrbitError;
use orbit_types::task::{TaskRelation, TaskRelationType, TaskStatus};
use tempfile::TempDir;

use super::super::{BindWorkspaceParams, RegisterWorkspaceParams};
use super::{bind, create_canonical_bundle, envelope, store};
use crate::fs::path_safety::normalize_path;

#[test]
fn bind_workspace_is_idempotent_for_orbit_dir() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let first = bind(&store, temp.path());
    let second = store
        .bind_workspace(BindWorkspaceParams {
            partition_id: Some(first.partition_id.clone()),
            slug: "Changed".into(),
            repo_root: temp.path().join("."),
            workspace_path: temp.path().join("."),
            orbit_dir: temp.path().join(".orbit").join("..").join(".orbit"),
            repo_fingerprint: Some("changed".into()),
        })
        .expect("idempotent bind");

    assert_eq!(first.partition_id, second.partition_id);
    assert_eq!(first.partition_id, second.partition_id);
}

#[test]
fn publication_fingerprint_is_adopted_once_and_then_fails_closed() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let workspace = bind(&store, temp.path());
    assert_eq!(
        store
            .find_workspace_binding(&workspace.partition_id)
            .expect("find workspace")
            .expect("workspace")
            .repo_fingerprint,
        None
    );

    let recorded = store
        .record_workspace_repo_fingerprint(&workspace.partition_id, "ssh://source.test/orbit.git")
        .expect("record fingerprint");
    assert_eq!(
        recorded.repo_fingerprint.as_deref(),
        Some("ssh://source.test/orbit.git")
    );
    store
        .record_workspace_repo_fingerprint(&workspace.partition_id, "ssh://source.test/orbit.git")
        .expect("idempotent recording");

    let error = store
        .record_workspace_repo_fingerprint(&workspace.partition_id, "ssh://source.test/other.git")
        .expect_err("mismatch must fail");
    assert!(error.to_string().contains("different source-repository"));
}

#[test]
fn bind_workspace_rebinds_same_checkout_under_a_new_orbit_dir() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let first = bind(&store, temp.path());

    // A later process for the same logical checkout brings its own ephemeral
    // orbit dir (ORB-10507). The bind must move the existing checkout binding
    // instead of failing with "already has a local checkout".
    let ephemeral_orbit_dir = temp.path().join(".orbit-ephemeral");
    fs::create_dir_all(&ephemeral_orbit_dir).expect("create ephemeral orbit dir");
    let second = store
        .bind_workspace(BindWorkspaceParams {
            partition_id: Some(first.partition_id.clone()),
            slug: "Orbit Test".into(),
            repo_root: temp.path().to_path_buf(),
            workspace_path: temp.path().to_path_buf(),
            orbit_dir: ephemeral_orbit_dir.clone(),
            repo_fingerprint: None,
        })
        .expect("rebind under a new orbit dir");

    assert_eq!(second.partition_id, first.partition_id);
    assert_eq!(second.orbit_dir, normalize_path(&ephemeral_orbit_dir));
    assert_eq!(
        store
            .find_workspace_checkout(&first.partition_id)
            .expect("find checkout")
            .expect("checkout exists")
            .orbit_dir,
        normalize_path(&ephemeral_orbit_dir),
        "the moved binding is the workspace's only checkout row"
    );
}

#[test]
fn bind_workspace_reuses_derived_id_for_same_paths_under_a_new_orbit_dir() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let repo_root = temp.path().join("repo");
    let first_orbit_dir = repo_root.join(".orbit");
    fs::create_dir_all(&first_orbit_dir).expect("create orbit dir");
    let derive = |orbit_dir: PathBuf| BindWorkspaceParams {
        partition_id: None,
        slug: "Orbit Test".into(),
        repo_root: repo_root.clone(),
        workspace_path: repo_root.clone(),
        orbit_dir,
        repo_fingerprint: None,
    };

    let first = store
        .bind_workspace(derive(first_orbit_dir))
        .expect("derive first binding");
    let ephemeral_orbit_dir = repo_root.join(".orbit-ephemeral");
    fs::create_dir_all(&ephemeral_orbit_dir).expect("create ephemeral orbit dir");
    let second = store
        .bind_workspace(derive(ephemeral_orbit_dir.clone()))
        .expect("derive second binding");

    assert_eq!(
        second.partition_id, first.partition_id,
        "the same checkout paths resolve back to one logical workspace"
    );
    assert_eq!(second.orbit_dir, normalize_path(&ephemeral_orbit_dir));
}

#[test]
fn bind_workspace_rejects_reusing_an_id_for_a_different_checkout() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let first = bind(&store, temp.path());

    let other_root = temp.path().join("other-repo");
    let other_orbit_dir = other_root.join(".orbit");
    fs::create_dir_all(&other_orbit_dir).expect("create other orbit dir");
    let result = store.bind_workspace(BindWorkspaceParams {
        partition_id: Some(first.partition_id.clone()),
        slug: "Orbit Test".into(),
        repo_root: other_root.clone(),
        workspace_path: other_root,
        orbit_dir: other_orbit_dir,
        repo_fingerprint: None,
    });

    assert!(matches!(
        result,
        Err(OrbitError::Store(message)) if message.contains("already has a local checkout")
    ));
}

#[test]
fn bind_workspace_rejects_explicit_workspace_id_conflict() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    bind(&store, temp.path());

    let result = store.bind_workspace(BindWorkspaceParams {
        partition_id: Some("other-abcdef".into()),
        slug: "Changed".into(),
        repo_root: temp.path().join("."),
        workspace_path: temp.path().join("."),
        orbit_dir: temp.path().join(".orbit").join("..").join(".orbit"),
        repo_fingerprint: Some("changed".into()),
    });

    assert!(matches!(result, Err(OrbitError::InvalidInput(_))));
}

#[test]
fn rebind_checkout_moves_orbit_dir_onto_the_requested_workspace() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let data_dir = temp.path().join("data");
    let parent = temp.path().to_path_buf();
    fs::create_dir_all(&data_dir).expect("create data dir");
    let synthetic = store
        .bind_workspace(BindWorkspaceParams {
            partition_id: Some("tmp-5b7149".into()),
            slug: "tmp".into(),
            repo_root: parent.clone(),
            workspace_path: parent,
            orbit_dir: data_dir.clone(),
            repo_fingerprint: None,
        })
        .expect("mint synthetic parent bind");

    let repo_root = temp.path().join("repo");
    fs::create_dir_all(&repo_root).expect("create repo");
    let rebound = store
        .rebind_checkout(BindWorkspaceParams {
            partition_id: Some("ws_qa".into()),
            slug: "qa".into(),
            repo_root: repo_root.clone(),
            workspace_path: repo_root.clone(),
            orbit_dir: data_dir.clone(),
            repo_fingerprint: None,
        })
        .expect("rebind orbit dir");

    assert_eq!(rebound.partition_id, "ws_qa");
    assert_eq!(rebound.repo_root, normalize_path(&repo_root));
    assert_eq!(rebound.orbit_dir, normalize_path(&data_dir));
    assert!(
        store
            .find_workspace_checkout(&synthetic.partition_id)
            .expect("lookup synthetic")
            .is_none(),
        "the synthetic checkout row must not keep the data dir"
    );
    let by_orbit = store
        .find_rebind_candidates(&repo_root, &repo_root, &data_dir)
        .expect("lookup rebound orbit dir");
    assert_eq!(by_orbit.len(), 1);
    assert_eq!(by_orbit[0].partition_id, "ws_qa");
}

#[test]
fn rebind_candidates_match_normalized_paths() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let workspace = bind(&store, temp.path());

    let candidates = store
        .find_rebind_candidates(
            &temp.path().join("."),
            &temp.path().join("nested").join(".."),
            &workspace.orbit_dir.join("..").join(".orbit"),
        )
        .expect("candidates");

    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].partition_id, workspace.partition_id);
}

#[test]
fn workspace_ids_lists_every_bound_partition() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let workspace = bind(&store, temp.path());
    store
        .register_workspace(RegisterWorkspaceParams {
            partition_id: "ws_remote".into(),
            slug: "remote".into(),
            repo_fingerprint: None,
        })
        .expect("register logical workspace");

    let ids = store.partition_ids().expect("list workspace ids");
    assert!(ids.contains(&workspace.partition_id));
    assert!(ids.contains("ws_remote"));
}

/// Deleting a workspace's bundle partition on disk is paired with retiring its
/// registry rows, so no binding survives naming a directory that is gone
/// [ORB-12119].
#[test]
fn unbind_workspace_retires_the_checkout_and_its_task_bundles() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let workspace = bind(&store, temp.path());
    let bundle_dir = create_canonical_bundle(&store, &workspace, "ORB-00000");
    store
        .register_task_bundle("ORB-00000", &workspace.partition_id, &bundle_dir)
        .expect("register bundle");
    store
        .replace_task_index(
            &workspace.partition_id,
            &envelope(
                "ORB-00000",
                TaskStatus::Backlog,
                vec!["v2".into()],
                Vec::new(),
            ),
        )
        .expect("index task");

    assert!(
        store
            .unbind_workspace(&workspace.partition_id)
            .expect("unbind workspace")
    );

    assert!(
        store
            .find_workspace_binding(&workspace.partition_id)
            .expect("read workspace binding")
            .is_none()
    );
    assert!(
        store
            .find_checkout_by_orbit_dir(&workspace.orbit_dir)
            .expect("read checkout binding")
            .is_none()
    );
    assert!(
        store
            .find_task_binding("ORB-00000")
            .expect("read task binding")
            .is_none(),
        "a task binding must not outlive its partition"
    );
    assert!(
        !store
            .unbind_workspace(&workspace.partition_id)
            .expect("second unbind is a no-op"),
        "unbinding an unknown workspace reports that nothing was retired"
    );
}

/// A relation another workspace points at the retired workspace's tasks must
/// not outlive the partition those tasks lived in [ORB-12119].
#[test]
fn unbind_workspace_retires_relations_pointing_into_it() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let retired = bind(&store, temp.path());

    let other_root = temp.path().join("other");
    let other_orbit_dir = other_root.join(".orbit");
    fs::create_dir_all(&other_orbit_dir).expect("create other orbit dir");
    let other = store
        .bind_workspace(BindWorkspaceParams {
            partition_id: Some("other-654321".into()),
            slug: "Other".into(),
            repo_root: other_root.clone(),
            workspace_path: other_root,
            orbit_dir: other_orbit_dir,
            repo_fingerprint: None,
        })
        .expect("bind other workspace");

    for (workspace_id, task_id) in [
        (&retired.partition_id, "ORB-00000"),
        (&other.partition_id, "ORB-00001"),
    ] {
        let bundle_dir = store
            .canonical_task_bundle_path(workspace_id, task_id)
            .expect("canonical bundle path");
        fs::create_dir_all(&bundle_dir).expect("create bundle");
        store
            .register_task_bundle(task_id, workspace_id, &bundle_dir)
            .expect("register bundle");
    }
    store
        .replace_task_index(
            &other.partition_id,
            &envelope(
                "ORB-00001",
                TaskStatus::Backlog,
                Vec::new(),
                vec![TaskRelation {
                    relation_type: TaskRelationType::BlockedBy,
                    target: "ORB-00000".into(),
                }],
            ),
        )
        .expect("index the pointing task");

    store
        .unbind_workspace(&retired.partition_id)
        .expect("unbind workspace");

    let conn = store.conn.lock().expect("lock registry");
    let remaining: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM task_bundle_relations WHERE target_task_id = ?1",
            ["ORB-00000"],
            |row| row.get(0),
        )
        .expect("count relations");
    assert_eq!(remaining, 0, "no edge may point into a retired partition");
}
