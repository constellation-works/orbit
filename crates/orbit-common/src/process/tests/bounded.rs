use std::fs;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use crate::OrbitError;
use crate::process::output_capture::OUTPUT_TRUNCATED_MARKER;
use crate::process::shell::quote_posix_arg;
use crate::process::{run_bounded, run_bounded_capped};

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
#[test]
fn bounded_run_returns_when_a_descendant_holds_either_pipe() {
    // Each case leaves a descendant holding exactly one of the output pipes
    // after the leader exits.
    for (label, redirect) in [("stdout", "2>/dev/null"), ("stderr", ">/dev/null")] {
        let dir = tempfile::tempdir().expect("tempdir");
        let child_path = dir.path().join("child.pid");
        let script = dir.path().join("orphan.sh");
        let body = format!(
            "#!/bin/sh\necho ready\nsleep 120 {redirect} &\necho $! > {}\nexit 0\n",
            quote_posix_arg(&child_path.display().to_string()),
        );
        fs::write(&script, body).expect("script");

        let deadline = Duration::from_secs(10);
        let started = Instant::now();
        let mut command = Command::new("sh");
        command.arg(&script);
        let output = run_bounded(&mut command, deadline).expect("leader exit is a finished wait");
        let elapsed = started.elapsed();
        assert!(
            elapsed < deadline,
            "{label}: returned in {elapsed:?}, past the {deadline:?} budget"
        );
        assert!(output.status.success(), "{label}: {:?}", output.status);
        assert_eq!(output.stdout, b"ready\n", "{label}");
        assert_reaped(read_pid(&child_path), b"sleep");
    }
}

#[cfg(unix)]
#[test]
fn bounded_run_drains_output_past_pipe_capacity_with_capped_retention() {
    // 1 MiB per stream is far past any pipe buffer: an undrained child would
    // block on write and hit the deadline instead of exiting.
    let mut command = Command::new("sh");
    command.args([
        "-c",
        "head -c 1048576 /dev/zero; head -c 1048576 /dev/zero >&2; echo done >&2",
    ]);
    let limit = 4096;
    let output =
        run_bounded_capped(&mut command, Duration::from_secs(30), limit).expect("drained run");
    assert!(output.status.success(), "{:?}", output.status);
    for (label, stream) in [("stdout", &output.stdout), ("stderr", &output.stderr)] {
        assert_eq!(
            stream.len(),
            limit + OUTPUT_TRUNCATED_MARKER.len(),
            "{label} retention"
        );
        assert!(stream[..limit].iter().all(|byte| *byte == 0), "{label}");
        assert!(stream.ends_with(OUTPUT_TRUNCATED_MARKER), "{label}");
    }
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
