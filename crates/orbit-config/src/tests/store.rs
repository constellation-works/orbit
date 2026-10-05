use crate::store::*;
use orbit_common::OrbitError;
use std::fs;
use tempfile::tempdir;

fn config_path(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join("config.toml")
}

#[test]
fn set_rejects_invalid_value_and_leaves_file_byte_identical() {
    let dir = tempdir().expect("tempdir");
    let path = config_path(dir.path());
    let original = "[execution.codex]\nsandbox = \"workspace-write\"\n";
    fs::write(&path, original).expect("write config");

    let mut store = ConfigStore::open(ConfigScope::Workspace, &path).expect("open store");
    store
        .set_value("execution.codex.sandbox", "not-a-real-mode")
        .expect("set_value only mutates the in-memory document");

    let error = store
        .validate()
        .expect_err("invalid sandbox mode must fail validation");
    assert!(matches!(error, OrbitError::InvalidInput(_)), "{error}");

    // `set_value`/`validate` never touch disk; only `save` (which the
    // caller must not call after a failed `validate`) does. Confirm the
    // file on disk is untouched, byte for byte.
    let after = fs::read(&path).expect("read config after failed validate");
    assert_eq!(after, original.as_bytes());
}

#[cfg(unix)]
#[test]
fn open_refuses_a_symlinked_config_file() {
    let dir = tempdir().expect("tempdir");
    let real = dir.path().join("real.toml");
    fs::write(&real, "[workflow]\nbase_branch = \"agent-main\"\n").expect("write real config");
    let link = config_path(dir.path());
    std::os::unix::fs::symlink(&real, &link).expect("symlink config leaf");

    let error = match ConfigStore::open(ConfigScope::Workspace, &link) {
        Err(error) => error,
        Ok(_) => panic!("a symlinked config file must be refused"),
    };
    assert!(
        error
            .to_string()
            .contains("config path must not be a symlink"),
        "{error}"
    );
}
