//! The pilot's origin fetch takes no fetch lock, so the deadline is what
//! bounds it: a remote that never answers is killed at the fetch deadline and
//! the fetch fails instead of hanging (fault injection, STD-03 §R22).
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use crate::git_test_support::run as run_git;
use orbit_common::process::shell::quote_posix_arg;
use orbit_common::test_env::{assert_child_test_passed, run_child_test};
use tempfile::TempDir;

use super::super::fetch_origin_branch;

/// Set in the re-executed child to the directory holding the stub `git`.
const STUB_DIR_VAR: &str = "ORBIT_TEST_HUNG_FETCH_DIR";

#[test]
fn hung_origin_fetch_is_killed_at_its_deadline() {
    const TEST: &str = concat!(
        "application::task::pilot::source::tests::source::",
        "hung_origin_fetch_is_killed_at_its_deadline"
    );
    if let Some(stub_dir) = std::env::var_os(STUB_DIR_VAR) {
        hung_fetch_child(Path::new(&stub_dir));
        return;
    }

    let real_git = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .expect("locate git");
    assert!(real_git.status.success(), "git is not on PATH");
    let real_git = String::from_utf8(real_git.stdout).expect("git path UTF-8");

    // Only `fetch` hangs; every other command reaches the real Git. PATH is
    // process-global, so the stub is installed only in a child.
    let stub_dir = TempDir::new().expect("stub dir");
    let quoted = |name: &str| quote_posix_arg(&stub_dir.path().join(name).display().to_string());
    let stub = stub_dir.path().join("git");
    fs::write(
        &stub,
        format!(
            "#!/bin/sh\nif [ \"$1\" = fetch ]; then\n  echo $$ > {}\n  sleep 600 &\n  echo $! > {}\n  wait\n  exit 1\nfi\nexec {} \"$@\"\n",
            quoted("leader.pid"),
            quoted("child.pid"),
            quote_posix_arg(real_git.trim()),
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

fn hung_fetch_child(stub_dir: &Path) {
    let repo = TempDir::new().expect("repo");
    for args in [
        &["init", "-q", "--template=", "-b", "main"][..],
        &["remote", "add", "origin", "/nonexistent/origin.git"],
    ] {
        let output = run_git(repo.path(), args).expect("git setup");
        assert!(output.success, "git {args:?}: {}", output.stderr);
    }

    let deadline = Duration::from_millis(500);
    let started = Instant::now();
    let fetched = fetch_origin_branch("task_pilot.prepare", repo.path(), "main", deadline);
    let elapsed = started.elapsed();
    assert!(
        elapsed < deadline + Duration::from_secs(5),
        "the hung fetch ran {elapsed:?}, far past the {deadline:?} fetch deadline"
    );
    let error = fetched.expect_err("a hung fetch must fail, never read as fetched");
    assert!(
        error.to_string().contains("timed out fetching origin/main"),
        "the failure names the timeout: {error}"
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
