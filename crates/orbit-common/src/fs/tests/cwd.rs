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

#[test]
fn inside_checkout_is_ok() {
    let (_root, checkout) = checkout_fixture();
    let nested = checkout.join("src");
    std::fs::create_dir_all(&nested).expect("nested dir");

    let resolved = confine(&nested.display().to_string(), &checkout, &[])
        .expect("a path inside the checkout is allowed");
    assert_eq!(
        resolved,
        nested.canonicalize().expect("canonicalize nested")
    );
}

#[test]
fn inside_linked_worktree_is_ok() {
    let (root, checkout) = checkout_fixture();
    let worktree = root.path().join("worktrees/jrun-fixture");
    std::fs::create_dir_all(&worktree).expect("linked worktree");
    let worktree = worktree.canonicalize().expect("canonicalize worktree");

    let resolved = confine(
        &worktree.display().to_string(),
        &checkout,
        &[worktree.parent().expect("worktrees dir").to_path_buf()],
    )
    .expect("a path inside a registered linked worktree is allowed");
    assert_eq!(resolved, worktree);
}

#[test]
fn sibling_checkout_is_refused() {
    let (root, checkout) = checkout_fixture();
    let sibling = root.path().join("sibling");
    std::fs::create_dir_all(&sibling).expect("sibling checkout");
    let requested = sibling.display().to_string();

    let message = outside_message(
        confine(&requested, &checkout, &[]).expect_err("a sibling checkout is outside"),
    );
    assert!(
        message.contains("outside workspace 'ws_fixture' checkout"),
        "{message}"
    );
    assert!(
        message.contains(&checkout.display().to_string()),
        "{message}"
    );
    assert!(message.contains("working_directory"), "{message}");
}

#[test]
fn home_is_refused() {
    let (_root, checkout) = checkout_fixture();
    let home = crate::fs::path::home_dir().expect("home directory");
    let home = if home.exists() {
        home.canonicalize().expect("canonicalize home")
    } else {
        home
    };
    assert!(
        !home.starts_with(&checkout),
        "the test home must lie outside the fixture checkout"
    );

    let message = outside_message(
        confine(&home.display().to_string(), &checkout, &[])
            .expect_err("$HOME is outside the checkout"),
    );
    assert!(message.contains("outside workspace"), "{message}");
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

#[test]
fn relative_path_is_refused() {
    let (_root, checkout) = checkout_fixture();
    let message =
        outside_message(confine("src", &checkout, &[]).expect_err("relative paths are refused"));
    assert!(message.contains("must be an absolute path"), "{message}");
    assert!(message.contains("'src'"), "{message}");
}
