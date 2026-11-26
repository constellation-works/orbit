use std::fs;

use super::super::socket::{BROKER_DIR, RunSocketDir};

#[test]
fn a_symlinked_broker_directory_is_refused() {
    let root = super::short_tempdir();
    let elsewhere = super::short_tempdir();
    fs::create_dir(root.path().join("state")).expect("state");
    std::os::unix::fs::symlink(elsewhere.path(), root.path().join(BROKER_DIR))
        .expect("plant symlink");

    let error = RunSocketDir::create(root.path()).expect_err("symlinked broker root");

    assert!(error.to_string().contains("symlink"), "{error}");
    assert_eq!(
        fs::read_dir(elsewhere.path()).expect("target").count(),
        0,
        "nothing may be created through the link"
    );
}

#[test]
fn a_symlinked_state_directory_is_refused() {
    let root = super::short_tempdir();
    let elsewhere = super::short_tempdir();
    std::os::unix::fs::symlink(elsewhere.path(), root.path().join("state")).expect("plant symlink");

    let error = RunSocketDir::create(root.path()).expect_err("symlinked state");

    assert!(error.to_string().contains("symlink"), "{error}");
    assert!(!elsewhere.path().join("plugin-broker").exists());
}
