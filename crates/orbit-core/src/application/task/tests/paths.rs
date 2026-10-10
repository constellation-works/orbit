//! Context selector and path validation coverage. Task context selectors are
//! always canonicalized against the repository root, and operator surfaces
//! reject selectors whose filesystem anchor does not exist. A `symbol:` name
//! and kind are not looked up.

use orbit_common::OrbitError;
use orbit_store::maintenance::task_registry::{
    BindWorkspaceParams, TaskRegistryStore, task_registry_path, write_workspace_config,
};
use tempfile::tempdir;

use super::{assert_isolated_child, enter_isolated_child};
use crate::OrbitRuntime;

/// Run one selector through the operator-surface guard and return the
/// `InvalidInput` message it must produce.
fn expect_selector_rejection(runtime: &OrbitRuntime, selector: &str) -> String {
    match runtime.ensure_context_selectors_exist(&[selector.to_string()]) {
        Err(OrbitError::InvalidInput(message)) => message,
        other => panic!("expected InvalidInput for `{selector}`, got {other:?}"),
    }
}

/// Shared-root / `--root` layout: tasks are stored for a registered checkout
/// even when this runtime open has no cwd binding. The guard must use that
/// checkout, not `parent(orbit-root)`.
fn explicit_root_runtime() -> (tempfile::TempDir, OrbitRuntime) {
    assert_isolated_child();
    let root = tempdir().expect("create tempdir");
    let data_dir = root.path().join("orbit-root");
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    std::fs::create_dir_all(repo.join("src")).expect("create repo src");
    std::fs::write(repo.join("src/main.rs"), b"fn main() {}\n").expect("write source");
    std::fs::write(data_dir.join("config.toml"), b"[workflow]\n").expect("write orbit config");

    write_workspace_config(
        &data_dir,
        &orbit_store::maintenance::task_registry::WorkspaceConfig {
            schema_version: 1,
            workspace_id: "ws_repo".to_string(),
        },
    )
    .expect("write workspace identity");
    TaskRegistryStore::open(&task_registry_path(&data_dir))
        .expect("open task registry")
        .bind_workspace(BindWorkspaceParams {
            partition_id: Some("ws_repo".to_string()),
            slug: "repo".to_string(),
            repo_root: repo,
            workspace_path: root.path().join("repo"),
            orbit_dir: data_dir.clone(),
            repo_fingerprint: None,
        })
        .expect("bind stored checkout");

    let runtime =
        OrbitRuntime::from_roots(&data_dir, &data_dir).expect("build explicit-root runtime");
    (root, runtime)
}

#[test]
fn explicit_root_without_cwd_binding_rejects_data_dir_and_parent_selectors() {
    if !enter_isolated_child(
        module_path!(),
        "explicit_root_without_cwd_binding_rejects_data_dir_and_parent_selectors",
    ) {
        return;
    }
    let (_root, runtime) = explicit_root_runtime();

    for selector in ["dir:orbit-root", "dir:repo", "file:config.toml"] {
        let message = expect_selector_rejection(&runtime, selector);
        assert!(
            message.contains(selector),
            "error must name selector: {message}"
        );
        assert!(
            message.contains("does not resolve to an existing in-workspace target"),
            "data-dir/parent paths must not be treated as in-workspace targets: {message}"
        );
    }
}

/// Deterministic interleaving: a strict write screened a declared target
/// against the task's grant, then another writer revoked that grant before
/// this write committed. The write must fail instead of storing a missing
/// target nothing authorizes; a declaration over the per-task bound is
/// refused outright.
#[test]
fn a_screened_write_is_refused_when_the_grant_it_relied_on_changes() {
    if !enter_isolated_child(
        module_path!(),
        "a_screened_write_is_refused_when_the_grant_it_relied_on_changes",
    ) {
        return;
    }
    let (_root, runtime) = explicit_root_runtime();
    let declared = vec![
        "file:src/main.rs".to_string(),
        "file:src/new.rs".to_string(),
    ];
    let task = runtime
        .add_task(crate::application::task::TaskAddParams {
            title: "Declared target".to_string(),
            description: "Creates src/new.rs.".to_string(),
            context_creation: runtime
                .authorize_missing_context(&declared)
                .expect("declare"),
            context_files: declared.clone(),
            ..Default::default()
        })
        .expect("add task");

    let screened = runtime
        .ensure_context_selectors_exist_for_update(&task.id, &declared)
        .expect("the declared target passes the strict check");
    let revoke = vec!["file:src/main.rs".to_string()];
    runtime
        .update_task(
            &task.id,
            crate::application::task::TaskUpdateParams {
                context_creation: runtime
                    .ensure_context_selectors_exist_for_update(&task.id, &revoke)
                    .expect("screen revocation"),
                context_files: Some(revoke.clone()),
                ..Default::default()
            },
        )
        .expect("revoke");
    let stale = runtime.update_task(
        &task.id,
        crate::application::task::TaskUpdateParams {
            context_files: Some(declared),
            context_creation: screened,
            ..Default::default()
        },
    );
    assert!(
        matches!(stale, Err(OrbitError::InvalidInput(_))),
        "{stale:?}"
    );
    assert_eq!(
        runtime.get_task(&task.id).expect("task").context_files,
        revoke
    );

    let too_many = (0..=orbit_types::task::MAX_CONTEXT_CREATION_SELECTORS)
        .map(|index| format!("file:src/new_{index}.rs"))
        .collect::<Vec<_>>();
    assert!(runtime.authorize_missing_context(&too_many).is_err());
}
