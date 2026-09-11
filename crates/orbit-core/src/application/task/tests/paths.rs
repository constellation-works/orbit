//! ORB-10475: `normalize_workspace_path` error coverage. The field is a
//! repository-relative/absolute filesystem path (never a logical/bridge
//! workspace id), and its errors must say so with a concrete example.

use orbit_common::OrbitError;
use tempfile::tempdir;

use crate::application::task::paths::{
    normalize_context_files_for_write, normalize_workspace_path, task_path_exists,
};

fn expect_invalid_input(result: Result<Option<String>, OrbitError>) -> String {
    match result {
        Err(OrbitError::InvalidInput(message)) => message,
        other => panic!("expected InvalidInput, got {other:?}"),
    }
}

#[test]
fn no_workspace_input_resolves_to_none() {
    let repo_root = tempdir().expect("create repo root");

    let resolved =
        normalize_workspace_path(repo_root.path(), None).expect("missing workspace is optional");

    assert_eq!(resolved, None);
}

#[test]
fn repository_root_path_is_accepted() {
    let repo_root = tempdir().expect("create repo root");

    let resolved = normalize_workspace_path(repo_root.path(), Some("."))
        .expect("repo root path is a valid workspace");

    let canonical_repo_root = repo_root
        .path()
        .canonicalize()
        .expect("canonicalize repo root");
    assert_eq!(
        resolved,
        Some(canonical_repo_root.to_string_lossy().into_owned())
    );
}

#[test]
fn absolute_repository_path_is_accepted() {
    let repo_root = tempdir().expect("create repo root");
    let canonical_repo_root = repo_root
        .path()
        .canonicalize()
        .expect("canonicalize repo root");

    let resolved = normalize_workspace_path(
        repo_root.path(),
        Some(canonical_repo_root.to_str().unwrap()),
    )
    .expect("absolute repo path is a valid workspace");

    assert_eq!(
        resolved,
        Some(canonical_repo_root.to_string_lossy().into_owned())
    );
}

#[test]
fn logical_workspace_id_is_rejected_with_a_path_form_error() {
    let repo_root = tempdir().expect("create repo root");

    // `ws_orbit` reads like a bridge/broker logical workspace id; it is not a
    // directory under the repo root, so it must fail — but the failure must
    // explain that a *path* was expected, not merely that the directory is
    // missing.
    let message =
        expect_invalid_input(normalize_workspace_path(repo_root.path(), Some("ws_orbit")));

    assert!(
        message.contains("filesystem path"),
        "error must state the accepted form is a filesystem path: {message}"
    );
    assert!(
        message.contains("logical workspace id"),
        "error must call out that a logical/bridge id is not accepted: {message}"
    );
    assert!(
        !message.to_lowercase().contains("workspace id must"),
        "error must never describe a logical id as the required form: {message}"
    );
}

#[test]
fn out_of_repository_path_is_rejected_naming_the_repository() {
    let repo_root = tempdir().expect("create repo root");
    let outside = tempdir().expect("create outside dir");
    let canonical_outside = outside.path().canonicalize().expect("canonicalize outside");

    let message = expect_invalid_input(normalize_workspace_path(
        repo_root.path(),
        Some(canonical_outside.to_str().unwrap()),
    ));

    assert!(
        message.contains("inside repository"),
        "error must name that the path must stay inside the repository: {message}"
    );
    let canonical_repo_root = repo_root
        .path()
        .canonicalize()
        .expect("canonicalize repo root");
    assert!(
        message.contains(canonical_repo_root.to_string_lossy().as_ref()),
        "error must cite the repository root it must stay inside: {message}"
    );
}

#[test]
fn path_to_a_file_is_rejected_as_not_a_directory() {
    let repo_root = tempdir().expect("create repo root");
    let file_path = repo_root.path().join("not-a-dir.txt");
    std::fs::write(&file_path, b"content").expect("write file");

    let message = expect_invalid_input(normalize_workspace_path(
        repo_root.path(),
        Some("not-a-dir.txt"),
    ));

    assert!(
        message.contains("directory"),
        "error must state a directory is required: {message}"
    );
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

#[test]
fn normalize_context_files_rejects_missing_selectors() {
    let workspace = tempdir().expect("create workspace");

    let error = normalize_context_files_for_write(
        vec!["file:does/not/exist.rs".to_string()],
        workspace.path(),
    )
    .expect_err("missing file must be rejected");

    match error {
        OrbitError::InvalidInput(msg) => {
            assert!(
                msg.contains("file:does/not/exist.rs"),
                "error must name selector: {msg}"
            );
        }
        other => panic!("expected InvalidInput, got {other:?}"),
    }

    let symbol_error = normalize_context_files_for_write(
        vec!["symbol:does/not/exist.rs#run:function".to_string()],
        workspace.path(),
    )
    .expect_err("missing symbol anchor must be rejected");

    match symbol_error {
        OrbitError::InvalidInput(msg) => {
            assert!(
                msg.contains("symbol:does/not/exist.rs#run:function"),
                "error must name selector: {msg}"
            );
        }
        other => panic!("expected InvalidInput, got {other:?}"),
    }
}

#[test]
fn normalize_context_files_rejects_target_kind_mismatch() {
    let workspace = tempdir().expect("create workspace");
    std::fs::create_dir_all(workspace.path().join("src")).expect("create src");
    std::fs::write(workspace.path().join("src/lib.rs"), b"pub fn run() {}\n")
        .expect("write anchor");

    // file: pointing to a directory
    let error = normalize_context_files_for_write(vec!["file:src".to_string()], workspace.path())
        .expect_err("file: targeting a directory must be rejected");
    match error {
        OrbitError::InvalidInput(msg) => {
            assert!(msg.contains("file:src"), "{msg}");
            assert!(msg.contains("file/directory kind"), "{msg}");
        }
        other => panic!("expected InvalidInput, got {other:?}"),
    }

    // dir: pointing to a file
    let error =
        normalize_context_files_for_write(vec!["dir:src/lib.rs".to_string()], workspace.path())
            .expect_err("dir: targeting a file must be rejected");
    match error {
        OrbitError::InvalidInput(msg) => {
            assert!(msg.contains("dir:src/lib.rs"), "{msg}");
            assert!(msg.contains("file/directory kind"), "{msg}");
        }
        other => panic!("expected InvalidInput, got {other:?}"),
    }

    // symbol: pointing to a directory
    let error = normalize_context_files_for_write(
        vec!["symbol:src#run:function".to_string()],
        workspace.path(),
    )
    .expect_err("symbol: targeting a directory must be rejected");
    match error {
        OrbitError::InvalidInput(msg) => {
            assert!(msg.contains("symbol:src#run:function"), "{msg}");
            assert!(msg.contains("file/directory kind"), "{msg}");
        }
        other => panic!("expected InvalidInput, got {other:?}"),
    }
}

#[test]
fn normalize_context_files_rejects_unsupported_kinds_and_malformed_selectors() {
    let workspace = tempdir().expect("create workspace");

    // module: selector is rejected
    let error = normalize_context_files_for_write(
        vec!["module:orbit_core::task".to_string()],
        workspace.path(),
    )
    .expect_err("module: selector must be rejected");
    match error {
        OrbitError::InvalidInput(msg) => {
            assert!(msg.contains("module:orbit_core::task"), "{msg}");
            assert!(msg.contains("must use file:, dir:, or symbol:"), "{msg}");
        }
        other => panic!("expected InvalidInput, got {other:?}"),
    }

    // command: selector is rejected
    let error =
        normalize_context_files_for_write(vec!["command:task".to_string()], workspace.path())
            .expect_err("command: selector must be rejected");
    match error {
        OrbitError::InvalidInput(msg) => {
            assert!(msg.contains("command:task"), "{msg}");
            assert!(msg.contains("must use file:, dir:, or symbol:"), "{msg}");
        }
        other => panic!("expected InvalidInput, got {other:?}"),
    }

    // malformed symbol selector is rejected
    let error = normalize_context_files_for_write(
        vec!["symbol:serve_throttled_response".to_string()],
        workspace.path(),
    )
    .expect_err("malformed symbol: must be rejected");
    match error {
        OrbitError::InvalidInput(msg) => {
            assert!(msg.contains("symbol:serve_throttled_response"), "{msg}");
        }
        other => panic!("expected InvalidInput, got {other:?}"),
    }
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
