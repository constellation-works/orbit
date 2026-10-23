#![cfg(unix)]
//! A granted path is resolved once, and the directory a rule binds to is the
//! one that resolution named, through exported boundary grant compilation.

use orbit_exec::{
    EnvironmentMode, ExecRequest, LandlockBoundary, StdinMode, linux_landlock_boundary_grants,
};

#[cfg(unix)]
#[test]
fn a_granted_write_root_standing_as_a_dangling_symlink_is_refused() {
    let temp = tempfile::tempdir().expect("tempdir");
    let base = temp.path().join("base");
    std::fs::create_dir_all(&base).expect("base");
    let root = base.join("dangling");
    std::os::unix::fs::symlink(temp.path().join("nowhere"), &root).expect("symlink");

    let error = linux_landlock_boundary_grants(
        &ExecRequest {
            program: "/bin/true".to_string(),
            args: vec![],
            current_dir: None,
            timeout_ms: None,
            stdin_mode: StdinMode::Null,
            environment_mode: EnvironmentMode::ClearAndSet(vec![]),
            debug: false,
        },
        &LandlockBoundary {
            write: vec![root],
            ..Default::default()
        },
    )
    .expect_err("a link standing where a directory is granted must not be followed")
    .to_string();
    assert!(error.contains("symbolic link"), "{error}");
}
