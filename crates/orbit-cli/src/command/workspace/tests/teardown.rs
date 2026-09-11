//! [ORB-12109] `workspace teardown` must not leave the deregistered
//! workspace's global task-store partition behind, and `orbit doctor` must
//! flag it if it ever does.

use std::path::Path;

use chrono::Utc;
use orbit_cmd::DoctorCommands;
use orbit_cmd::task_store::task_workspaces_dir;
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
fn teardown_deletes_the_task_store_partition_and_doctor_confirms_no_orphan_remains() {
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
    write_task_bundle(&global_root, "ws_teardown", "ORB-2");
    write_task_bundle(&global_root, "ws_teardown", "ORB-3");
    let partition = task_workspaces_dir(&global_root).join("ws_teardown");
    assert!(
        partition.is_dir(),
        "fixture task store must exist before teardown"
    );

    let runtime = OrbitRuntime::from_roots(&global_root, &orbit_dir).expect("build runtime");
    WorkspaceTeardownArgs { confirm: true }
        .execute(&runtime)
        .expect("teardown");

    assert!(
        !partition.exists(),
        "teardown must delete the torn-down workspace's task store"
    );
    assert!(
        task_workspaces_dir(&global_root)
            .join("ws_survivor")
            .exists(),
        "teardown must not touch another workspace's task store"
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
    // partitions — the torn-down partition is gone, and the survivor's is
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
