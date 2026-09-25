//! Sibling tests for `config_path.rs`: selecting the effective `config.toml`
//! from a runtime's roots.

use tempfile::tempdir;

use super::runtime::test_runtime;
use crate::runtime::existing_config_file_path;

#[test]
fn config_path_prefers_existing_workspace_config_over_global() {
    let (_root, runtime, global_root, workspace_root) = test_runtime();
    std::fs::write(global_root.join("config.toml"), "").expect("write global config");
    std::fs::write(workspace_root.join("config.toml"), "").expect("write workspace config");

    let expected = workspace_root
        .canonicalize()
        .expect("canonicalize workspace root")
        .join("config.toml");
    assert_eq!(runtime.config_path().expect("select config"), expected);
}

#[test]
fn config_path_falls_back_to_global_when_workspace_config_is_absent() {
    let (_root, runtime, global_root, _workspace_root) = test_runtime();

    assert_eq!(
        runtime.config_path().expect("select config"),
        global_root.join("config.toml")
    );
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

#[test]
fn config_path_rejects_a_non_regular_workspace_config() {
    let (_root, runtime, _global_root, workspace_root) = test_runtime();
    std::fs::create_dir(workspace_root.join("config.toml")).expect("create config directory");

    let error = runtime
        .config_path()
        .expect_err("non-regular workspace config must fail closed");

    assert!(error.to_string().contains("regular config.toml"), "{error}");
}

#[test]
fn workspace_config_selection_reports_a_non_directory_root() {
    let root = tempdir().expect("create tempdir");
    let root_file = root.path().join("not-a-directory");
    std::fs::write(&root_file, "not a directory").expect("write root file");

    let error = existing_config_file_path(&root_file)
        .expect_err("a config child cannot be selected beneath a file");

    assert!(error.to_string().contains("failed to inspect config path"));
    assert!(error.to_string().contains("not-a-directory/config.toml"));
}

#[test]
fn config_root_validation_treats_a_missing_root_as_absent() {
    let root = tempdir().expect("create tempdir");
    let missing = root.path().join("missing");

    assert_eq!(
        existing_config_file_path(&missing).expect("select beneath missing root"),
        None
    );
}

#[test]
#[cfg(unix)]
fn workspace_config_selection_accepts_a_trusted_root_alias() {
    use std::os::unix::fs::symlink;

    let root = tempdir().expect("create tempdir");
    let real_root = root.path().join("real");
    let alias_root = root.path().join("alias");
    std::fs::create_dir(&real_root).expect("create real root");
    std::fs::write(real_root.join("config.toml"), "").expect("write config");
    symlink(&real_root, &alias_root).expect("create trusted root alias");

    let selected_config = existing_config_file_path(&alias_root)
        .expect("select through trusted alias")
        .expect("config exists");

    assert_eq!(
        selected_config,
        real_root
            .canonicalize()
            .expect("canonical root")
            .join("config.toml")
    );
}
