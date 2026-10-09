//! `run_git`'s deadline at its public boundary: a Git that never exits is
//! killed with its process group and reported as a timeout naming the argv
//! and the workspace, never as Git's answer.
#![cfg(unix)]
#![allow(missing_docs, clippy::expect_used, clippy::unwrap_used)]

orbit_common::isolate_test_process!();

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use orbit_common::OrbitError;
use orbit_common::fs::git::run_git_within;
use orbit_common::process::shell::quote_posix_arg;
use orbit_common::test_env::{assert_child_test_passed, run_child_test};
use tempfile::TempDir;

/// Set in the re-executed child to the directory holding the stub `git`.
const STUB_DIR_VAR: &str = "ORBIT_TEST_HUNG_GIT_DIR";

#[test]
fn hung_git_times_out_and_leaves_no_child() {
    const TEST: &str = "hung_git_times_out_and_leaves_no_child";
    if let Some(stub_dir) = std::env::var_os(STUB_DIR_VAR) {
        hung_git_child(Path::new(&stub_dir));
        return;
    }

    // PATH is process-global, so the stub is installed only in a child.
    let stub_dir = TempDir::new().expect("stub dir");
    let quoted = |name: &str| quote_posix_arg(&stub_dir.path().join(name).display().to_string());
    let stub = stub_dir.path().join("git");
    fs::write(
        &stub,
        format!(
            "#!/bin/sh\necho $$ > {}\nsleep 600 &\necho $! > {}\nwait\n",
            quoted("leader.pid"),
            quoted("child.pid"),
        ),
    )
    .expect("write stub git");
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).expect("stub executable");
    let mut paths = vec![stub_dir.path().to_path_buf()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));

    let output_dir = TempDir::new().expect("child output dir");
    let mut command = Command::new(std::env::current_exe().expect("test binary"));
    command
        .args(["--exact", TEST, "--nocapture"])
        .env(STUB_DIR_VAR, stub_dir.path())
        .env("PATH", std::env::join_paths(paths).expect("stub PATH"));
    let output = run_child_test(&mut command, TEST, output_dir.path());
    assert_child_test_passed(TEST, output.status, &output.stdout, &output.stderr);
}

fn hung_git_child(stub_dir: &Path) {
    let workspace = TempDir::new().expect("workspace");
    let deadline = Duration::from_millis(500);
    let started = Instant::now();
    let error = run_git_within(workspace.path(), &["rev-parse", "HEAD"], deadline)
        .err()
        .expect("a git that never exits must time out");
    let elapsed = started.elapsed();
    assert!(
        elapsed < deadline + Duration::from_secs(5),
        "returned after {elapsed:?}, far past the {deadline:?} deadline"
    );
    let OrbitError::ProcessTimeout { timeout_ms, detail } = error else {
        panic!("expected ProcessTimeout, got {error:?}");
    };
    assert_eq!(timeout_ms, 500);
    assert!(
        detail.contains("git rev-parse HEAD")
            && detail.contains(&workspace.path().display().to_string()),
        "timeout must name the argv and the workspace: {detail}"
    );

    // Only Linux exposes `/proc` to tell a survivor apart.
    if !cfg!(target_os = "linux") {
        return;
    }
    for (file, marker) in [("leader.pid", "git"), ("child.pid", "sleep")] {
        let pid: u32 = fs::read_to_string(stub_dir.join(file))
            .expect("stub recorded its pid")
            .trim()
            .parse()
            .expect("pid");
        assert_gone(pid, marker.as_bytes());
    }
}

/// The process is gone or a zombie, or its pid now belongs to something else.
fn assert_gone(pid: u32, marker: &[u8]) {
    let give_up = Instant::now() + Duration::from_secs(2);
    loop {
        match fs::read(format!("/proc/{pid}/cmdline")) {
            Err(_) => return,
            Ok(cmdline) if !cmdline.windows(marker.len()).any(|window| window == marker) => {
                return;
            }
            Ok(cmdline) if Instant::now() >= give_up => panic!(
                "pid {pid} survived the timeout: {}",
                String::from_utf8_lossy(&cmdline)
            ),
            Ok(_) => thread::sleep(Duration::from_millis(20)),
        }
    }
}
