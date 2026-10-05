use std::path::{Path, PathBuf};

use tempfile::TempDir;

use crate::OrbitError;
use crate::fs::cwd::confine_workspace_cwd;

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

fn outside_message(error: OrbitError) -> String {
    match error {
        OrbitError::InvalidInput(message) => message,
        other => panic!("expected invalid input, got {other:?}"),
    }
}

#[cfg(unix)]
#[test]
fn symlink_that_escapes_is_refused() {
    let (root, checkout) = checkout_fixture();
    let outside = root.path().join("outside");
    std::fs::create_dir_all(&outside).expect("outside dir");
    let link = checkout.join("escape");
    std::os::unix::fs::symlink(&outside, &link).expect("symlink that escapes");

    let message = outside_message(
        confine(&link.display().to_string(), &checkout, &[])
            .expect_err("a symlink resolving outside the checkout is refused"),
    );
    assert!(message.contains("outside workspace"), "{message}");
}
