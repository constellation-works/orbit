//! Working-directory confinement for `orbit.command.exec`.
//!
//! `confine_workspace_cwd` refuses a symlink whose target leaves the checkout.
//! The managed-run dispatch denial for this tool is covered at the tool-host
//! boundary in `orbit-core`.

use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::cwd::confine_workspace_cwd;
use tempfile::TempDir;

fn checkout_fixture() -> (TempDir, PathBuf) {
    let root = TempDir::new().expect("tempdir");
    let checkout = root.path().join("repo");
    std::fs::create_dir_all(&checkout).expect("checkout");
    let checkout = checkout.canonicalize().expect("canonicalize checkout");
    (root, checkout)
}

fn confine(
    requested: &str,
    checkout: &Path,
    extra_roots: &[PathBuf],
) -> Result<PathBuf, OrbitError> {
    confine_workspace_cwd(
        "working_directory",
        requested,
        "ws_fixture",
        checkout,
        extra_roots,
    )
}

fn invalid_input(error: OrbitError) -> String {
    match error {
        OrbitError::InvalidInput(message) => message,
        other => panic!("expected invalid input, got {other:?}"),
    }
}

#[cfg(unix)]
#[test]
fn working_directory_symlink_that_escapes_is_refused() {
    let (root, checkout) = checkout_fixture();
    let outside = root.path().join("outside");
    std::fs::create_dir_all(&outside).expect("outside dir");
    let link = checkout.join("escape");
    std::os::unix::fs::symlink(&outside, &link).expect("escaping symlink");

    let message = invalid_input(
        confine(&link.display().to_string(), &checkout, &[]).expect_err("escaping symlink"),
    );
    assert!(message.contains("outside workspace"), "{message}");
}
