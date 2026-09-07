#![allow(missing_docs)]
#![cfg(unix)]
// Integration fixtures exercise public behavior and unwrap setup invariants.
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! Ctrl-C must terminate every live supervised process group, not only the
//! one that happened to hold the (former) process-wide handler mutex.
//!
//! This lives in its own test binary so `raise(SIGINT)` cannot interrupt
//! other `orbit-exec` unit tests sharing a process.

use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use orbit_exec::{EnvironmentMode, ExecRequest, NoSandbox, StdinMode, run_process};

#[test]
fn ctrl_c_terminates_every_live_child_process_group() {
    let dir = tempfile::tempdir().expect("tempdir");
    let first_ready = dir.path().join("first.ready");
    let second_ready = dir.path().join("second.ready");

    let started = Instant::now();
    thread::scope(|scope| {
        let first = scope.spawn(|| supervise_sleep_after_ready(&first_ready));
        let second = scope.spawn(|| supervise_sleep_after_ready(&second_ready));
        wait_for_marker(&first_ready);
        wait_for_marker(&second_ready);

        // Safety: `raise` delivers SIGINT to this process. The supervised
        // wait has replaced the default disposition with Orbit's handler.
        let raised = unsafe { libc::raise(libc::SIGINT) };
        assert_eq!(raised, 0, "raise SIGINT");

        let first = first.join().expect("first supervisor thread");
        let second = second.join().expect("second supervisor thread");
        assert_interrupted(&first);
        assert_interrupted(&second);
    });

    assert!(
        started.elapsed() < Duration::from_secs(2),
        "SIGINT should terminate both live children promptly, took {:?}",
        started.elapsed()
    );
}

fn supervise_sleep_after_ready(marker: &Path) -> orbit_exec::ExecutionResult {
    let script = format!("touch {} && sleep 8", marker.display());
    let req = ExecRequest {
        program: "/bin/sh".to_string(),
        args: vec!["-c".to_string(), script],
        current_dir: None,
        timeout_ms: Some(15_000),
        stdin_mode: StdinMode::Null,
        environment_mode: EnvironmentMode::Inherit,
        debug: false,
    };
    run_process(&req, &NoSandbox).expect("run_process")
}

fn wait_for_marker(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if path.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("child did not become ready: {}", path.display());
}

fn assert_interrupted(result: &orbit_exec::ExecutionResult) {
    assert!(!result.success, "interrupted child must not succeed");
    assert_eq!(
        result.exit_code,
        Some(128 + libc::SIGINT),
        "parent-signal exits report 128+SIGINT, got {:?}",
        result.exit_code
    );
    assert!(
        result
            .stderr
            .contains("process interrupted by signal SIGINT"),
        "stderr was {:?}",
        result.stderr
    );
}
