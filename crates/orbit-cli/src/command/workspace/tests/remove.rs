use chrono::Utc;
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
