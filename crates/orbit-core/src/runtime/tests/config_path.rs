//! Sibling tests for `config_path.rs`: selecting the effective `config.toml`
//! from a runtime's roots.

use std::path::PathBuf;

use tempfile::tempdir;

use crate::OrbitRuntime;

fn test_runtime() -> (tempfile::TempDir, OrbitRuntime, PathBuf, PathBuf) {
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

#[test]
#[cfg(unix)]
fn config_path_rejects_a_symlink_without_reading_its_external_target() {
    use std::os::unix::fs::symlink;

    let (_root, runtime, global_root, workspace_root) = test_runtime();
    let outside_target = workspace_root
        .parent()
        .expect("workspace root has parent")
        .join("outside-config.toml");
    std::fs::write(
        global_root.join("config.toml"),
        "[scoring]\nenabled = false\n",
    )
    .expect("write global config");
    std::fs::write(&outside_target, "leaked = true\n").expect("write file outside workspace root");
    symlink(&outside_target, workspace_root.join("config.toml"))
        .expect("symlink workspace config.toml");

    let error = runtime
        .config_path()
        .expect_err("symlinked workspace config must fail closed");

    let diagnostic = error.to_string();
    assert!(diagnostic.contains("regular config.toml"), "{diagnostic}");
    assert!(diagnostic.contains(&workspace_root.display().to_string()));
    assert!(global_root.join("config.toml").is_file());
}
