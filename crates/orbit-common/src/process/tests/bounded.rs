use std::fs;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use crate::OrbitError;
use crate::process::shell::quote_posix_arg;
use crate::process::{CapturedOutput, run_bounded};

#[test]
fn bounded_run_returns_status_and_output() {
    let mut command = Command::new("sh");
    command.args(["-c", "echo hello; echo err >&2; exit 3"]);
    let CapturedOutput {
        status,
        stdout,
        stderr,
    } = run_bounded(&mut command, Duration::from_secs(5)).expect("wait");
    assert_eq!(status.code(), Some(3));
    assert!(!status.success());
    assert_eq!(stdout, b"hello\n");
    assert_eq!(stderr, b"err\n");
}

#[test]
fn bounded_run_reports_a_spawn_failure() {
    let mut command = Command::new("/no/such/orbit-bounded-runner");
    let error = run_bounded(&mut command, Duration::from_secs(1)).expect_err("spawn");
    assert!(matches!(error, OrbitError::Execution(_)), "{error}");
}

#[cfg(unix)]
#[test]
fn bounded_run_times_out_and_reaps_the_process_group() {
    let dir = tempfile::tempdir().expect("tempdir");
    let leader_path = dir.path().join("leader.pid");
    let child_path = dir.path().join("child.pid");
    let script = dir.path().join("stall.sh");
    let body = format!(
        "#!/bin/sh\necho $$ > {}\nsleep 120 &\necho $! > {}\nwait\n",
        quote_posix_arg(&leader_path.display().to_string()),
        quote_posix_arg(&child_path.display().to_string()),
    );
    fs::write(&script, body).expect("script");

    let deadline = Duration::from_millis(400);
    let started = Instant::now();
    let mut command = Command::new("sh");
    command.arg(&script);
    let error = run_bounded(&mut command, deadline).expect_err("timeout");
    let elapsed = started.elapsed();
    assert!(
        elapsed >= deadline,
        "returned in {elapsed:?}, before the {deadline:?} budget"
    );
    assert!(
        elapsed < deadline + Duration::from_secs(2),
        "returned in {elapsed:?}, past the {deadline:?} budget"
    );
    match error {
        OrbitError::ProcessTimeout { timeout_ms, .. } => {
            assert_eq!(timeout_ms, u64::try_from(deadline.as_millis()).unwrap());
        }
        other => panic!("expected process timeout, got {other}"),
    }

    let leader = read_pid(&leader_path);
    let child = read_pid(&child_path);
    assert_reaped(leader, script.to_str().unwrap().as_bytes());
    assert_reaped(child, b"sleep");
}

#[cfg(unix)]
fn read_pid(path: &std::path::Path) -> u32 {
    fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("pid file {}: {error}", path.display()))
        .trim()
        .parse()
        .expect("pid")
}

/// The recorded process is gone, or its pid was reused by something else.
#[cfg(unix)]
fn assert_reaped(pid: u32, marker: &[u8]) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match fs::read(format!("/proc/{pid}/cmdline")) {
            Err(_) => return,
            Ok(cmdline) if !cmdline.windows(marker.len()).any(|window| window == marker) => {
                return;
            }
            Ok(cmdline) if Instant::now() >= deadline => {
                panic!(
                    "pid {pid} still running: {}",
                    String::from_utf8_lossy(&cmdline)
                );
            }
            Ok(_) => thread::sleep(Duration::from_millis(20)),
        }
    }
}
