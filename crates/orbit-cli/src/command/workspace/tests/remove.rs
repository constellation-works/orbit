use chrono::Utc;
use orbit_cmd::DoctorCommands;
use orbit_cmd::WorkspaceDoctorStatus;
use orbit_cmd::task_store::{partition_is_bound, task_workspaces_dir};
use orbit_core::OrbitRuntime;
use orbit_registry::workspace_registry;
use orbit_types::workspace::{Workspace, WorkspaceCheckout, WorkspaceStatus};
use tempfile::tempdir;

use crate::command::Execute;

use super::super::remove::WorkspaceRemoveArgs;

#[test]
fn remove_accepts_the_recorded_path_of_a_deleted_checkout() {
    let temp = tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let survivor_orbit = temp.path().join("survivor").join(".orbit");
    let deleted_repo = temp.path().join("deleted");
    let deleted_orbit = deleted_repo.join(".orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&survivor_orbit).expect("create survivor runtime");

    let registry_path = workspace_registry::registry_path_for(&global_root);
    let mut registry =
        workspace_registry::load_registry_from(&registry_path).expect("load registry");
    let now = Utc::now();
    workspace_registry::register_workspace(
        &mut registry,
        Workspace {
            id: "ws_deleted".to_string(),
            name: "deleted".to_string(),
            owner_machine_id: None,
            git_remote: None,
            ship_mode: None,
            base_branch: "main".to_string(),
            status: WorkspaceStatus::Invalid,
            created_at: now,
            updated_at: now,
        },
    )
    .expect("register deleted workspace");
    workspace_registry::register_checkout(
        &mut registry,
        WorkspaceCheckout::owner("ws_deleted".to_string(), deleted_repo, deleted_orbit),
    )
    .expect("register deleted checkout");
    workspace_registry::save_registry_to(&registry, &registry_path).expect("save registry");

    let runtime = OrbitRuntime::from_roots(&global_root, &survivor_orbit).expect("runtime");
    WorkspaceRemoveArgs {
        workspace: temp.path().join("deleted").to_string_lossy().into_owned(),
    }
    .execute(&runtime)
    .expect("remove deleted checkout");

    let registry =
        workspace_registry::load_registry_from(&registry_path).expect("load updated registry");
    assert!(
        workspace_registry::find_workspace_by_id(&registry, "ws_deleted").is_none(),
        "path removal must deregister the missing workspace"
    );
    assert!(
        registry
            .checkouts
            .iter()
            .all(|checkout| checkout.workspace_id != "ws_deleted"),
        "path removal must remove the checkout binding too"
    );
}

fn register_workspace(
    global_root: &std::path::Path,
    workspace_id: &str,
    name: &str,
    repo_root: &std::path::Path,
    orbit_dir: &std::path::Path,
) {
    let registry_path = workspace_registry::registry_path_for(global_root);
    let mut registry =
        workspace_registry::load_registry_from(&registry_path).expect("load registry");
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
            status: WorkspaceStatus::Invalid,
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

fn write_task_bundle(global_root: &std::path::Path, workspace_id: &str, task_id: &str) {
    let bundle = task_workspaces_dir(global_root)
        .join(workspace_id)
        .join(task_id);
    std::fs::create_dir_all(&bundle).expect("create task bundle dir");
    std::fs::write(bundle.join("task.yaml"), b"id: dummy\n").expect("write bundle file");
}

fn orphan_row(results: &[orbit_cmd::WorkspaceDoctorResult]) -> &orbit_cmd::WorkspaceDoctorResult {
    results
        .iter()
        .find(|row| row.check_name == "orphan-task-stores")
        .expect("orphan-task-stores row present")
}

/// [ORB-12223] Shared external-root layout: deleted checkout → doctor warns →
/// `workspace remove` → doctor still reports the leftover → repair reclaims it.
#[test]
fn shared_root_remove_keeps_a_deleted_checkout_partition_reclaimable() {
    let temp = tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let survivor = temp.path().join("survivor");
    let deleted = temp.path().join("deleted");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&survivor).expect("create survivor checkout");
    std::fs::create_dir_all(&deleted).expect("create deleted checkout");

    register_workspace(
        &global_root,
        "ws_survivor",
        "survivor",
        &survivor,
        &global_root,
    );
    register_workspace(
        &global_root,
        "ws_deleted",
        "deleted",
        &deleted,
        &global_root,
    );
    write_task_bundle(&global_root, "ws_deleted", "ORB-2");
    write_task_bundle(&global_root, "ws_deleted", "ORB-3");
    std::fs::remove_dir_all(&deleted).expect("delete checkout");

    let runtime = OrbitRuntime::from_resolved_roots(&global_root, &global_root, &survivor)
        .expect("shared-root runtime");

    let results = runtime.doctor_workspace().expect("doctor before remove");
    let row = orphan_row(&results);
    assert_eq!(row.status, WorkspaceDoctorStatus::Warning, "{row:?}");
    assert!(row.message.contains("ws_deleted"), "{}", row.message);
    assert!(
        row.message.contains("missing checkout directories"),
        "{}",
        row.message
    );

    WorkspaceRemoveArgs {
        workspace: "ws_deleted".to_string(),
    }
    .execute(&runtime)
    .expect("remove deleted shared-root workspace");

    assert!(
        task_workspaces_dir(&global_root)
            .join("ws_deleted")
            .join("ORB-2")
            .is_dir(),
        "remove must not delete task bundles"
    );

    let results = runtime.doctor_workspace().expect("doctor after remove");
    let row = orphan_row(&results);
    assert_eq!(
        row.status,
        WorkspaceDoctorStatus::Warning,
        "catalog removal must not re-hide the partition as claimed: {row:?}"
    );
    assert!(row.message.contains("ws_deleted"), "{}", row.message);
    assert!(
        row.message.contains("missing checkout directories"),
        "{}",
        row.message
    );

    let removed = runtime
        .remove_orphan_task_stores()
        .expect("repair after workspace remove");
    assert_eq!(removed.populated_partitions, 1, "{removed:?}");
    assert_eq!(removed.task_bundles, 2, "{removed:?}");
    assert!(
        !task_workspaces_dir(&global_root)
            .join("ws_deleted")
            .exists()
    );
    assert!(!partition_is_bound(&global_root, "ws_deleted").expect("read binding"));
}
