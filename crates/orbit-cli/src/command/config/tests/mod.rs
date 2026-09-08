use std::path::PathBuf;

use orbit_core::OrbitRuntime;
use tempfile::tempdir;

mod get;
mod set;
mod show;
mod support;

/// Build a runtime with independent global/workspace roots so tests can
/// control exactly which of the two `config.toml` files exists on disk.
/// Mirrors `command::task::tests::artifact::test_runtime`.
pub(super) fn test_runtime() -> (tempfile::TempDir, OrbitRuntime, PathBuf, PathBuf) {
    let root = tempdir().expect("create tempdir");
    let global_root = root.path().join("global");
    let repo_root = root.path().join("repo");
    let workspace_root = repo_root.join(".orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    let runtime =
        OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build test runtime");
    (root, runtime, global_root, workspace_root)
}
