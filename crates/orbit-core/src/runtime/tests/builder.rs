//! Sibling tests for `builder.rs` (migrated per ORB-00246 / docs/design-patterns/test_layout.md).

use std::path::PathBuf;

use orbit_store::maintenance::task_registry::{
    BindWorkspaceParams, TaskRegistryStore, read_workspace_config_optional, task_registry_path,
    write_workspace_config,
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
    write_workspace_config(
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

    write_workspace_config(
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
    assert_eq!(
        runtime.context.paths().repo_root,
        data_dir,
        "an unbound data-dir open must not treat parent(data-dir) as repo_root"
    );
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

/// ORB-12222: a registered workspace sharing an explicit data dir still has a
/// checkout row. Opening that data dir without a cwd binding must recover that
/// checkout instead of minting parent(data-dir) as `repo_root`.
#[test]
fn explicit_data_dir_runtime_recovers_the_stored_checkout_repo_root() {
    let root = tempdir().expect("tempdir");
    let data_dir = root.path().join("orbit-root");
    let repo_root = root.path().join("repo");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    std::fs::create_dir_all(repo_root.join("src")).expect("create repo");
    std::fs::write(repo_root.join("src/main.rs"), b"fn main() {}\n").expect("write source");

    write_workspace_config(
        &data_dir,
        &orbit_store::maintenance::task_registry::WorkspaceConfig {
            schema_version: 1,
            workspace_id: "ws_repo".to_string(),
        },
    )
    .expect("write workspace config");
    let registry =
        TaskRegistryStore::open(&task_registry_path(&data_dir)).expect("open task registry");
    let bound = registry
        .bind_workspace(BindWorkspaceParams {
            workspace_id: Some("ws_repo".to_string()),
            slug: "repo".to_string(),
            repo_root: repo_root.clone(),
            workspace_path: repo_root.clone(),
            orbit_dir: data_dir.clone(),
            repo_fingerprint: None,
        })
        .expect("bind stored checkout");

    let runtime = OrbitRuntime::from_roots(&data_dir, &data_dir).expect("build data-dir runtime");
    assert_eq!(
        runtime.context.paths().repo_root,
        bound.repo_root,
        "explicit-root open without a cwd binding must use the stored checkout, not parent(data-dir)"
    );
    assert_ne!(
        runtime.context.paths().repo_root,
        data_dir.parent().expect("data dir parent"),
        "parent(data-dir) must not become repo_root for a registered workspace"
    );
}

/// A read-only state mount carries no writable state directory, so an optional
/// semantic index that was never built there cannot be created at startup.
/// Opening the runtime and every read that does not need that index must still
/// work; semantic ranking must name the unavailable index instead of answering
/// as a complete but empty corpus.
#[cfg(unix)]
#[test]
fn absent_semantic_index_on_unwritable_state_keeps_the_runtime_observational() {
    use std::os::unix::fs::PermissionsExt;

    use orbit_search::SemanticSearchParams;

    use crate::application::search::{
        GlobalSearchKind, GlobalSearchMode, GlobalSearchParams, GlobalSearchResponse,
    };

    let (_root, global_root, workspace_root, runtime) = v2_runtime();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Observational needle".to_string(),
            description: "Readable through a state directory that refuses writes".to_string(),
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("create task on writable state");
    drop(runtime);

    let state_dir = workspace_root.join("state");
    let semantic_db = state_dir.join("semantic.db");
    assert!(
        semantic_db.exists(),
        "writable initialization must create the semantic index"
    );
    for suffix in ["", "-wal", "-shm"] {
        let sidecar = PathBuf::from(format!("{}{suffix}", semantic_db.display()));
        if sidecar.exists() {
            std::fs::remove_file(&sidecar).expect("remove the index this workspace never built");
        }
    }
    let original = std::fs::metadata(&state_dir)
        .expect("state metadata")
        .permissions();
    std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o500))
        .expect("make the state directory read-only");
    if std::fs::File::create(state_dir.join("probe")).is_ok() {
        // A user that ignores the mode bits (typically root) cannot reproduce
        // the denial this fixture is about.
        std::fs::set_permissions(&state_dir, original).expect("restore state permissions");
        return;
    }

    let observed = observe_unwritable_state(&global_root, &workspace_root, &task.id);
    std::fs::set_permissions(&state_dir, original).expect("restore state permissions");
    assert!(
        !semantic_db.exists(),
        "observing an unavailable index must not create it"
    );

    let (listed, semantic, hybrid) = observed;
    assert_eq!(
        listed,
        vec![task.id.clone()],
        "task reads must still answer"
    );

    let message = semantic.to_string();
    assert!(
        message.contains("semantic index") && message.contains("is unavailable"),
        "semantic search must name the unavailable index: {message}"
    );
    assert_eq!(
        hybrid.mode,
        GlobalSearchMode::Lexical,
        "hybrid search must degrade rather than report an empty semantic corpus"
    );
    assert!(
        hybrid
            .notes
            .iter()
            .any(|note| note.contains("falling back to lexical task search")
                && note.contains("is unavailable")),
        "the fallback must disclose why semantic ranking was skipped: {:?}",
        hybrid.notes
    );
    assert_eq!(
        hybrid.results.first().and_then(|hit| hit.id.as_deref()),
        Some(task.id.as_str()),
        "lexical ranking must still find the task"
    );

    /// Opens a second runtime while the state directory refuses writes and
    /// collects everything the assertions need, so the fixture can restore the
    /// directory's mode before any of them can unwind.
    fn observe_unwritable_state(
        global_root: &std::path::Path,
        workspace_root: &std::path::Path,
        task_id: &str,
    ) -> (Vec<String>, OrbitError, GlobalSearchResponse) {
        let runtime = OrbitRuntime::from_roots(global_root, workspace_root)
            .expect("an absent optional semantic index must not refuse the runtime");
        let listed = runtime
            .list_tasks()
            .expect("list tasks through the unwritable state directory")
            .into_iter()
            .map(|task| task.id)
            .collect();
        let semantic = runtime
            .semantic_search(SemanticSearchParams {
                query: "observational needle".to_string(),
                limit: 3,
                field: None,
                kind: None,
                model: None,
            })
            .expect_err("semantic search must refuse an unavailable index");
        let hybrid = runtime
            .global_search(GlobalSearchParams {
                query: Some("observational needle".to_string()),
                hybrid: true,
                kind: GlobalSearchKind::Task,
                limit: 3,
                ..Default::default()
            })
            .expect("hybrid search must fall back instead of failing");
        assert_eq!(
            runtime.get_task(task_id).expect("read the task").title,
            "Observational needle"
        );
        (listed, semantic, hybrid)
    }
}
