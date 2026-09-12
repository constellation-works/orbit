//! Context selector and path validation coverage. Task context selectors are
//! always canonicalized against the repository root, and operator surfaces
//! reject selectors whose filesystem anchor does not exist. A `symbol:` name
//! and kind are not looked up.

use orbit_common::OrbitError;
use orbit_store::maintenance::task_registry::{
    BindWorkspaceParams, TaskRegistryStore, task_registry_path, write_workspace_config,
};
use tempfile::tempdir;

use super::test_runtime;
use crate::OrbitRuntime;
use crate::application::task::paths::{
    canonicalize_context_files_for_read, normalize_context_files_for_write, task_path_exists,
};

/// Run one selector through the operator-surface guard and return the
/// `InvalidInput` message it must produce.
fn expect_selector_rejection(runtime: &OrbitRuntime, selector: &str) -> String {
    match runtime.ensure_context_selectors_exist(&[selector.to_string()]) {
        Err(OrbitError::InvalidInput(message)) => message,
        other => panic!("expected InvalidInput for `{selector}`, got {other:?}"),
    }
}

#[test]
fn normalize_context_files_accepts_valid_selectors() {
    let workspace = tempdir().expect("create workspace");
    std::fs::create_dir_all(workspace.path().join("src")).expect("create src");
    std::fs::write(workspace.path().join("src/lib.rs"), b"pub fn run() {}\n")
        .expect("write anchor");

    let selectors = vec![
        "file:src/lib.rs".to_string(),
        "dir:src".to_string(),
        "symbol:src/lib.rs#run:function".to_string(),
    ];

    let normalized = normalize_context_files_for_write(selectors.clone(), workspace.path())
        .expect("valid selectors must be accepted");

    assert_eq!(normalized, selectors);
}

/// The core write path canonicalizes without resolving the target: internal
/// callers record selectors for files the task is about to create, and symbol
/// metadata is opaque to the filesystem. Existence is an operator-surface rule
/// (see the `ensure_context_selectors_exist` tests below).
#[test]
fn normalize_context_files_keeps_selectors_that_do_not_exist_yet() {
    let workspace = tempdir().expect("create workspace");
    std::fs::create_dir_all(workspace.path().join("src")).expect("create src");
    std::fs::write(workspace.path().join("src/lib.rs"), b"pub fn run() {}\n")
        .expect("write anchor");

    let selectors = vec![
        "file:src/future.rs".to_string(),
        "symbol:src/lib.rs#not::a::real::symbol:invented-kind".to_string(),
    ];

    let normalized = normalize_context_files_for_write(selectors.clone(), workspace.path())
        .expect("not-yet-existing targets must survive the core write path");

    assert_eq!(normalized, selectors);
    assert!(task_path_exists(
        workspace.path(),
        "symbol:src/lib.rs#not::a::real::symbol:invented-kind"
    ));
    assert_eq!(
        canonicalize_context_files_for_read(&normalized, workspace.path()),
        normalized
    );
}

#[test]
fn ensure_context_selectors_exist_accepts_resolvable_selectors() {
    let (root, runtime) = test_runtime();
    let repo_root = root.path().join("repo");
    std::fs::create_dir_all(repo_root.join("src")).expect("create src");
    std::fs::write(repo_root.join("src/lib.rs"), b"pub fn run() {}\n").expect("write anchor");

    runtime
        .ensure_context_selectors_exist(&[
            "file:src/lib.rs".to_string(),
            "dir:src".to_string(),
            "symbol:src/lib.rs#run:function".to_string(),
        ])
        .expect("resolvable selectors must be accepted");
}

#[test]
fn ensure_context_selectors_exist_accepts_symbol_whose_name_is_absent() {
    let (root, runtime) = test_runtime();
    let repo_root = root.path().join("repo");
    std::fs::create_dir_all(repo_root.join("src")).expect("create src");
    std::fs::write(repo_root.join("src/lib.rs"), b"pub fn real_symbol() {}\n")
        .expect("write anchor");

    runtime
        .ensure_context_selectors_exist(&["symbol:src/lib.rs#no_such_symbol:fn".to_string()])
        .expect("an existing filesystem anchor is enough; the `symbol:` name is not verified");
}

#[test]
fn ensure_context_selectors_exist_rejects_missing_selectors() {
    let (_root, runtime) = test_runtime();

    let message = expect_selector_rejection(&runtime, "file:does/not/exist.rs");
    assert!(
        message.contains("file:does/not/exist.rs"),
        "error must name selector: {message}"
    );
    assert!(
        message.contains("only the filesystem anchor is verified"),
        "error must document that a `symbol:` name is not verified: {message}"
    );
    assert!(
        message.contains("not a `symbol:` name or kind"),
        "error must name the unverified `symbol:` half: {message}"
    );

    let message = expect_selector_rejection(&runtime, "symbol:does/not/exist.rs#run:function");
    assert!(
        message.contains("symbol:does/not/exist.rs#run:function"),
        "error must name selector: {message}"
    );
    assert!(
        message.contains("only the filesystem anchor is verified"),
        "error must document that a `symbol:` name is not verified: {message}"
    );
}

#[test]
fn ensure_context_selectors_exist_rejects_target_kind_mismatch() {
    let (root, runtime) = test_runtime();
    let repo_root = root.path().join("repo");
    std::fs::create_dir_all(repo_root.join("src")).expect("create src");
    std::fs::write(repo_root.join("src/lib.rs"), b"pub fn run() {}\n").expect("write anchor");

    for selector in ["file:src", "dir:src/lib.rs", "symbol:src#run:function"] {
        let message = expect_selector_rejection(&runtime, selector);
        assert!(message.contains(selector), "{message}");
        assert!(message.contains("file/directory kind"), "{message}");
    }
}

#[test]
fn ensure_context_selectors_exist_rejects_unsupported_kinds_and_malformed_selectors() {
    let (_root, runtime) = test_runtime();

    for selector in ["module:orbit_core::task", "command:task"] {
        let message = expect_selector_rejection(&runtime, selector);
        assert!(message.contains(selector), "{message}");
        assert!(
            message.contains("must use file:, dir:, or symbol:"),
            "{message}"
        );
    }

    let message = expect_selector_rejection(&runtime, "symbol:serve_throttled_response");
    assert!(
        message.contains("symbol:serve_throttled_response"),
        "{message}"
    );
}

#[test]
fn symbol_context_validation_rejects_missing_and_outside_anchors() {
    let workspace = tempdir().expect("create workspace");
    let outside = tempdir().expect("create outside root");
    let outside_file = outside.path().join("outside.rs");
    std::fs::write(&outside_file, b"fn outside() {}\n").expect("write outside anchor");

    assert!(!task_path_exists(
        workspace.path(),
        "symbol:src/missing.rs#run:function"
    ));
    assert!(!task_path_exists(
        workspace.path(),
        &format!("symbol:{}#run:function", outside_file.display())
    ));
}

/// Shared-root / `--root` layout: tasks are stored for a registered checkout
/// even when this runtime open has no cwd binding. The guard must use that
/// checkout, not `parent(orbit-root)`.
fn explicit_root_runtime() -> (tempfile::TempDir, OrbitRuntime) {
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
            workspace_id: Some("ws_repo".to_string()),
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
fn explicit_root_without_cwd_binding_accepts_the_stored_checkout_selector() {
    let (_root, runtime) = explicit_root_runtime();

    runtime
        .ensure_context_selectors_exist(&["file:src/main.rs".to_string()])
        .expect("a file in the stored checkout must be accepted");
}

#[test]
fn explicit_root_without_cwd_binding_rejects_data_dir_and_parent_selectors() {
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

#[test]
fn unbound_explicit_data_dir_refuses_selector_validation_without_a_checkout_binding() {
    let root = tempdir().expect("create tempdir");
    let data_dir = root.path().join("orbit-root");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    let runtime = OrbitRuntime::from_roots(&data_dir, &data_dir).expect("build unbound runtime");

    let message = expect_selector_rejection(&runtime, "file:config.toml");
    assert!(
        message.contains("no checkout binding"),
        "refusal must name the missing binding: {message}"
    );
    assert!(
        message.contains(data_dir.to_str().expect("utf8 data dir")),
        "refusal must name the Orbit root: {message}"
    );
}
