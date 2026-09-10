//! Sibling tests for `builder.rs` (migrated per ORB-00246 / docs/design-patterns/test_layout.md).

use std::path::PathBuf;

use orbit_store::maintenance::task_registry::{
    TaskRegistryStore, read_workspace_config_optional, task_registry_path,
};

use crate::OrbitError;

use orbit_common::NotFoundKind;
use orbit_types::task::TaskStatus;
use tempfile::tempdir;

use crate::OrbitRuntime;
use crate::application::task::{TaskAddParams, TaskUpdateParams};
use crate::application::workflow::ShipMode;
use crate::runtime::WorkspaceRuntimeBinding;

fn v2_runtime() -> (tempfile::TempDir, PathBuf, PathBuf, OrbitRuntime) {
    let root = tempdir().expect("tempdir");
    let global_root = root.path().join("global");
    let repo_root = root.path().join("repo");
    let workspace_root = repo_root.join(".orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    let runtime = OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build runtime");
    (root, global_root, workspace_root, runtime)
}

#[test]
fn in_memory_runtime_initializes_an_isolated_workspace() {
    let runtime = OrbitRuntime::in_memory().expect("build in-memory runtime");
    let other = OrbitRuntime::in_memory().expect("build second in-memory runtime");
    assert_eq!(
        runtime.workspace_id().expect("workspace identity"),
        "ws_memory"
    );

    let task = runtime
        .add_task(TaskAddParams {
            title: "In-memory task".to_string(),
            plan: "Start the task".to_string(),
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("create task in initialized partition");
    assert_eq!(runtime.list_tasks().expect("list tasks").len(), 1);
    assert!(other.list_tasks().expect("list isolated tasks").is_empty());

    let started = runtime
        .start_task(&task.id, Some("start".to_string()), None)
        .expect("start task with workspace lock reservations");
    assert_eq!(started.status, TaskStatus::InProgress);
}

#[test]
fn registry_neutral_binding_controls_workspace_id_repo_root_and_ship_mode() {
    let root = tempdir().expect("tempdir");
    let global_root = root.path().join("global");
    let custom_repo_root = root.path().join("custom-repo-root");
    let workspace_root = root.path().join("detached-orbit-root");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");

    let binding = WorkspaceRuntimeBinding {
        logical_workspace_id: "ws_bound".to_string(),
        workspace_id: "ws_bound".to_string(),
        owner_machine_id: None,
        repo_root: custom_repo_root.clone(),
        ship_mode: ShipMode::Pr,
        base_branch: None,
    };
    let runtime =
        OrbitRuntime::from_roots_with_binding(&global_root, &workspace_root, binding.clone())
            .expect("build bound runtime");

    assert_eq!(runtime.workspace_id().expect("workspace id"), "ws_bound");
    assert_eq!(runtime.context.paths().repo_root, custom_repo_root);
    assert_eq!(runtime.workspace_runtime_binding(), Some(&binding));
}

#[test]
fn registry_neutral_binding_rejects_a_conflicting_workspace_config() {
    let root = tempdir().expect("tempdir");
    let global_root = root.path().join("global");
    let workspace_root = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    orbit_store::maintenance::task_registry::write_workspace_config(
        &workspace_root,
        &orbit_store::maintenance::task_registry::WorkspaceConfig {
            schema_version: 1,
            workspace_id: "ws_configured".to_string(),
        },
    )
    .expect("write workspace config");

    let error = OrbitRuntime::from_roots_with_binding(
        &global_root,
        &workspace_root,
        WorkspaceRuntimeBinding {
            logical_workspace_id: "ws_other".to_string(),
            workspace_id: "ws_other".to_string(),
            owner_machine_id: None,
            repo_root: root.path().join("repo"),
            ship_mode: ShipMode::Local,
            base_branch: None,
        },
    )
    .err()
    .expect("conflicting binding must fail closed");

    assert!(
        error
            .to_string()
            .contains("does not match configured workspace id")
    );
}

#[test]
fn v2_task_backend_wires_through_runtime_add_show_list_and_update() {
    let (_root, global_root, _workspace_root, runtime) = v2_runtime();

    let task = runtime
        .add_task(TaskAddParams {
            title: "Runtime v2 task".to_string(),
            description: "Created through OrbitRuntime".to_string(),
            plan: "1. Start it".to_string(),
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("create task");
    assert_eq!(task.id, "ORB-00000");
    let bundle_path = global_root
        .join("tasks/workspaces")
        .join(runtime.workspace_id().expect("workspace identity"))
        .join(&task.id);
    assert!(bundle_path.exists());

    let started = runtime
        .start_task(&task.id, Some("start".to_string()), None)
        .expect("start task");
    assert_eq!(started.status, TaskStatus::InProgress);

    let updated = runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                comment: Some("Runtime comment".to_string()),
                execution_summary: Some("Finished the runtime smoke".to_string()),
                status: Some(TaskStatus::Review),
                ..Default::default()
            },
        )
        .expect("update task");
    assert_eq!(updated.status, TaskStatus::Review);
    assert!(
        runtime
            .get_task_comments(&task.id)
            .expect("read task comments")
            .iter()
            .any(|comment| comment.message == "Runtime comment")
    );
    assert_eq!(runtime.list_tasks().expect("list tasks").len(), 1);
    assert_eq!(
        runtime
            .search_tasks("runtime smoke")
            .expect("search tasks")
            .len(),
        1
    );

    runtime
        .delete_task_guarded(&updated.id, true)
        .expect("delete v2 task");
    assert!(matches!(
        runtime.get_task(&updated.id),
        Err(OrbitError::NotFound {
            kind: NotFoundKind::Task,
            ..
        })
    ));
}

#[test]
fn v2_task_backend_persists_workspace_binding_across_runtime_rebuild() {
    let (_root, global_root, workspace_root, runtime) = v2_runtime();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Persistent v2 task".to_string(),
            description: "Survives runtime reconstruction".to_string(),
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("create task");
    let workspace_config =
        read_workspace_config_optional(&workspace_root).expect("read workspace config");
    let workspace_id = workspace_config
        .as_ref()
        .map(|config| config.workspace_id.as_str())
        .expect("workspace id");
    assert!(workspace_id.starts_with("repo-"), "{workspace_id}");
    assert_eq!(workspace_id.len(), "repo-000000".len());

    let rebuilt = OrbitRuntime::from_roots(&global_root, &workspace_root).expect("rebuild runtime");
    let fetched = rebuilt.get_task(&task.id).expect("get task after rebuild");
    assert_eq!(fetched.title, "Persistent v2 task");
    assert_eq!(
        read_workspace_config_optional(&workspace_root)
            .expect("read workspace config")
            .map(|config| config.workspace_id),
        workspace_config.map(|config| config.workspace_id)
    );
}

#[test]
fn v2_task_backend_rebinds_when_workspace_config_is_missing() {
    let (_root, global_root, workspace_root, runtime) = v2_runtime();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Rebind v2 task".to_string(),
            description: "Survives missing workspace config".to_string(),
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("create task");
    let original_config =
        read_workspace_config_optional(&workspace_root).expect("read workspace config");
    std::fs::remove_file(workspace_root.join("config.yaml")).expect("remove workspace config");

    let rebuilt = OrbitRuntime::from_roots(&global_root, &workspace_root).expect("rebuild runtime");
    let fetched = rebuilt.get_task(&task.id).expect("get task after rebind");

    assert_eq!(fetched.title, "Rebind v2 task");
    assert_eq!(
        read_workspace_config_optional(&workspace_root)
            .expect("read rewritten workspace config")
            .map(|config| config.workspace_id),
        original_config.map(|config| config.workspace_id)
    );
}

/// ORB-10985: a checkout whose `config.yaml` identity drifted away from the
/// registry row for its orbit dir must still open — the bound partition holds
/// its task state — and the identity file is reconciled onto that partition.
#[test]
fn v2_task_backend_adopts_the_bound_workspace_when_the_checkout_identity_drifts() {
    let (_root, global_root, workspace_root, runtime) = v2_runtime();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Drifted identity task".to_string(),
            description: "Stays reachable after config.yaml is rewritten".to_string(),
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("create task");
    let bound_workspace_id = read_workspace_config_optional(&workspace_root)
        .expect("read workspace config")
        .map(|config| config.workspace_id)
        .expect("workspace id");
    drop(runtime);

    orbit_store::maintenance::task_registry::write_workspace_config(
        &workspace_root,
        &orbit_store::maintenance::task_registry::WorkspaceConfig {
            schema_version: 1,
            workspace_id: "orbit-5c61b3".to_string(),
        },
    )
    .expect("diverge the checkout identity");

    let rebuilt = OrbitRuntime::from_roots(&global_root, &workspace_root)
        .expect("a drifted checkout identity must not refuse the runtime");
    let fetched = rebuilt.get_task(&task.id).expect("get task after drift");
    assert_eq!(fetched.title, "Drifted identity task");
    assert_eq!(
        read_workspace_config_optional(&workspace_root)
            .expect("read reconciled workspace config")
            .map(|config| config.workspace_id),
        Some(bound_workspace_id),
        "the checkout identity must be reconciled onto the bound partition"
    );
}

#[test]
fn explicit_data_dir_runtime_does_not_bind_parent_as_a_checkout() {
    let root = tempdir().expect("tempdir");
    let data_dir = root.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");

    let runtime = OrbitRuntime::from_roots(&data_dir, &data_dir).expect("build data-dir runtime");
    drop(runtime);

    assert!(
        read_workspace_config_optional(&data_dir)
            .expect("read workspace config")
            .is_none(),
        "an unbound data-dir open must not write a synthetic workspace identity"
    );
    let registry =
        TaskRegistryStore::open(&task_registry_path(&data_dir)).expect("open task registry");
    let parent = data_dir.parent().expect("data dir parent");
    let candidates = registry
        .find_rebind_candidates(parent, parent, &data_dir)
        .expect("lookup checkout bindings");
    assert!(
        candidates.is_empty(),
        "executor-list-style data-dir open must not insert a checkout for parent(data-dir); got {candidates:?}"
    );
}
