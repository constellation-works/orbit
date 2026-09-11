//! [ORB-12109] `workspace teardown` must not leave the deregistered
//! workspace's global task-store partition behind, and `orbit doctor` must
//! flag it if it ever does.
//!
//! [ORB-12119] The partition to delete is the one the *task registry* binds to
//! the checkout, not the one named for its workspace-catalog id, and its
//! registry bindings must be retired with it.

use std::path::Path;

use chrono::Utc;
use orbit_cmd::DoctorCommands;
use orbit_cmd::task_store::{bound_partition_id, partition_is_bound, task_workspaces_dir};
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_registry::workspace_registry;
use orbit_types::workspace::{Workspace, WorkspaceCheckout, WorkspaceStatus};

use crate::command::Execute;

use super::super::teardown::WorkspaceTeardownArgs;

fn write_task_bundle(global_root: &Path, workspace_id: &str, task_id: &str) {
    let bundle = task_workspaces_dir(global_root)
        .join(workspace_id)
        .join(task_id);
    std::fs::create_dir_all(&bundle).expect("create task bundle dir");
    std::fs::write(bundle.join("task.yaml"), b"id: dummy\n").expect("write bundle file");
}

fn register(global_root: &Path, workspace_id: &str, repo_root: &Path, orbit_dir: &Path) {
    let registry_path = workspace_registry::registry_path_for(global_root);
    let mut registry =
        workspace_registry::load_registry_from(&registry_path).expect("load registry");
    let now = Utc::now();
    workspace_registry::register_workspace(
        &mut registry,
        Workspace {
            id: workspace_id.to_string(),
            name: workspace_id.to_string(),
            owner_machine_id: None,
            git_remote: None,
            ship_mode: None,
            base_branch: "main".to_string(),
            status: WorkspaceStatus::Active,
            created_at: now,
            updated_at: now,
        },
    )
    .expect("register workspace");
    workspace_registry::register_checkout(
        &mut registry,
        WorkspaceCheckout::owner(
            workspace_id.to_string(),
            repo_root.to_path_buf(),
            orbit_dir.to_path_buf(),
        ),
    )
    .expect("register checkout");
    workspace_registry::save_registry_to(&registry, &registry_path).expect("save registry");
}

#[test]
fn teardown_deletes_the_bound_task_store_partition_and_retires_its_bindings() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let repo_root = temp.path().join("repo");
    let orbit_dir = repo_root.join(".orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&orbit_dir).expect("create workspace root");

    // A second, untouched registered workspace with its own task bundle,
    // proving teardown and the doctor check both stay scoped to the
    // workspace actually torn down.
    let survivor_root = temp.path().join("survivor");
    let survivor_orbit_dir = survivor_root.join(".orbit");
    std::fs::create_dir_all(&survivor_orbit_dir).expect("create survivor workspace root");
    register(
        &global_root,
        "ws_survivor",
        &survivor_root,
        &survivor_orbit_dir,
    );
    write_task_bundle(&global_root, "ws_survivor", "ORB-1");

    register(&global_root, "ws_teardown", &repo_root, &orbit_dir);
    let runtime = OrbitRuntime::from_roots(&global_root, &orbit_dir).expect("build runtime");

    // Opening the runtime bound this checkout in the task registry. That
    // binding — not the catalog's `ws_teardown` — names the partition its task
    // state lives in.
    let bound = bound_partition_id(&global_root, &orbit_dir)
        .expect("read checkout binding")
        .expect("checkout is bound");
    assert_ne!(
        bound, "ws_teardown",
        "fixture must exercise the two distinct id spaces"
    );
    write_task_bundle(&global_root, &bound, "ORB-2");
    write_task_bundle(&global_root, &bound, "ORB-3");
    // A partition named for the catalog id, as an older binary would have left it.
    write_task_bundle(&global_root, "ws_teardown", "ORB-4");

    let bound_partition = task_workspaces_dir(&global_root).join(&bound);
    assert!(
        bound_partition.is_dir(),
        "fixture task store must exist before teardown"
    );

    WorkspaceTeardownArgs { confirm: true }
        .execute(&runtime)
        .expect("teardown");

    assert!(
        !bound_partition.exists(),
        "teardown must delete the partition this checkout's task state is bound to"
    );
    assert!(
        !task_workspaces_dir(&global_root)
            .join("ws_teardown")
            .exists(),
        "teardown must also delete a partition left under its catalog id"
    );
    assert!(
        task_workspaces_dir(&global_root)
            .join("ws_survivor")
            .exists(),
        "teardown must not touch another workspace's task store"
    );

    // No binding may survive pointing at a deleted bundle directory.
    assert!(
        !partition_is_bound(&global_root, &bound).expect("read workspace bindings"),
        "teardown must retire the task-registry binding for the deleted partition"
    );
    assert!(
        bound_partition_id(&global_root, &orbit_dir)
            .expect("read checkout binding")
            .is_none(),
        "teardown must retire the checkout binding for the deleted orbit dir"
    );

    let registry = workspace_registry::load_registry_from(&workspace_registry::registry_path_for(
        &global_root,
    ))
    .expect("load registry after teardown");
    assert!(
        workspace_registry::find_workspace_by_id(&registry, "ws_teardown").is_none(),
        "teardown must still deregister the workspace"
    );

    // Doctor, run right after teardown, must report no orphaned task-store
    // partitions — the torn-down partitions are gone, and the survivor's is
    // still registered.
    let results = runtime.doctor_workspace().expect("doctor after teardown");
    let row = results
        .iter()
        .find(|row| row.check_name == "orphan-task-stores")
        .expect("orphan-task-stores row present");
    assert_eq!(row.status, orbit_cmd::WorkspaceDoctorStatus::Ok, "{row:?}");
}

#[test]
fn teardown_without_confirm_leaves_the_task_store_and_registration_untouched() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let repo_root = temp.path().join("repo");
    let orbit_dir = repo_root.join(".orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&orbit_dir).expect("create workspace root");
    register(&global_root, "ws_unconfirmed", &repo_root, &orbit_dir);
    write_task_bundle(&global_root, "ws_unconfirmed", "ORB-1");

    let runtime = OrbitRuntime::from_roots(&global_root, &orbit_dir).expect("build runtime");
    let result = WorkspaceTeardownArgs { confirm: false }.execute(&runtime);
    assert!(matches!(result, Err(OrbitError::InvalidInput(_))));

    assert!(
        task_workspaces_dir(&global_root)
            .join("ws_unconfirmed")
            .exists(),
        "an unconfirmed teardown must not touch the task store"
    );
    let registry = workspace_registry::load_registry_from(&workspace_registry::registry_path_for(
        &global_root,
    ))
    .expect("load registry after unconfirmed teardown");
    assert!(
        workspace_registry::find_workspace_by_id(&registry, "ws_unconfirmed").is_some(),
        "an unconfirmed teardown must not deregister the workspace"
    );
}
