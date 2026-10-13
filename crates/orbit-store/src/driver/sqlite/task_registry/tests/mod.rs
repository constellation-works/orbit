use std::path::PathBuf;

use tempfile::TempDir;

use super::{TaskRegistryStore, task_registry_path};

fn registry_path(temp: &TempDir) -> PathBuf {
    task_registry_path(temp.path())
}

/// ORB-15070: a fresh open through a symlinked root must key the same
/// `workspaces_dir` as a later open of the canonical root. The repair gate
/// keys on that path, so a spelling split splits its attempt budget.
/// Admitted under criterion 3 (symlink confinement): `workspaces_dir` is
/// crate-private, so the public boundary cannot observe the key split.
#[cfg(unix)]
#[test]
fn fresh_open_through_symlinked_root_matches_reopen_by_canonical_root() {
    let real = TempDir::new().expect("tempdir");
    let alias_parent = TempDir::new().expect("tempdir");
    let alias = alias_parent.path().join("root");
    std::os::unix::fs::symlink(real.path(), &alias).expect("symlink root");

    let fresh = TaskRegistryStore::open(&task_registry_path(&alias)).expect("fresh open");
    let reopened = TaskRegistryStore::open(&registry_path(&real)).expect("reopen");

    assert_eq!(
        fresh.workspaces_dir(),
        reopened.workspaces_dir(),
        "a fresh and a reopened handle on one registry root must share one workspaces_dir"
    );
}
