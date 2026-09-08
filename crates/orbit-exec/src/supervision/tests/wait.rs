use std::time::{Duration, Instant};

use orbit_common::process::output_capture::OUTPUT_TRUNCATED_MARKER;

use super::super::wait::{WaitResult, wait_with_timeout_and_output_limit};
use crate::runner::{EnvironmentMode, ExecRequest, StdinMode};

#[cfg(unix)]
#[test]
fn wait_kills_process_when_stdout_capture_limit_is_exceeded() {
    let req = ExecRequest {
        program: "/bin/sh".to_string(),
        args: vec!["-c".to_string(), "yes orbit-cap".to_string()],
        current_dir: None,
        timeout_ms: Some(5_000),
        stdin_mode: StdinMode::Null,
        environment_mode: EnvironmentMode::Inherit,
        debug: false,
    };
    let child = crate::process::spawn(&req).expect("spawn child");

    let started = Instant::now();
    let result =
        wait_with_timeout_and_output_limit(child, Some(5_000), false, None, 64).expect("wait");

    assert!(!result.exit_success);
    // Hitting the output cap terminates the process two valid ways, decided by
    // thread scheduling: the supervisor may observe the cap signal first and
    // SIGKILL the group (exit_code None), or — under load — be parked in
    // child.wait_timeout when /bin/sh self-exits 141 because `yes` took SIGPIPE
    // the instant the drain thread dropped the pipe read end (128 + 13 = 141).
    // Both promptly kill the process and cap output (asserted below); only the
    // reported exit_code differs by who delivered the kill.
    assert!(
        result.exit_code.is_none() || result.exit_code == Some(141),
        "expected None (SIGKILL) or Some(141) (SIGPIPE cascade), got {:?}",
        result.exit_code
    );
    // `yes` may die of SIGPIPE before the supervisor observes the cap; only
    // the supervisor-delivered kill carries the reason (asserted separately
    // below with a child that survives the pipe closing).
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.is_empty() || stderr.contains("process output capture limit exceeded on stdout"),
        "stderr was {stderr:?}"
    );
    assert!(result.stdout.ends_with(OUTPUT_TRUNCATED_MARKER));
    assert!(result.stdout.len() <= 64 + OUTPUT_TRUNCATED_MARKER.len());
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "output cap should kill the subprocess promptly"
    );
}

/// A child that ignores SIGPIPE keeps running after the drain drops the pipe,
/// so the supervisor is the one that stops it — and it must say why, or the
/// caller sees a bare failure with an empty stderr for a log that was merely
/// long.
#[cfg(unix)]
#[test]
fn capture_limit_kill_names_its_reason_on_stderr() {
    let req = ExecRequest {
        program: "/bin/sh".to_string(),
        args: vec![
            "-c".to_string(),
            "trap '' PIPE; while :; do echo orbit-cap; done".to_string(),
        ],
        current_dir: None,
        timeout_ms: Some(5_000),
        stdin_mode: StdinMode::Null,
        environment_mode: EnvironmentMode::Inherit,
        debug: false,
    };
    let child = crate::process::spawn(&req).expect("spawn child");

    let result =
        wait_with_timeout_and_output_limit(child, Some(5_000), false, None, 64).expect("wait");

    assert!(!result.exit_success);
    assert_eq!(result.exit_code, None, "supervisor kill, not a self-exit");
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("process output capture limit exceeded on stdout"),
        "stderr was {stderr:?}"
    );
    assert!(result.stdout.ends_with(OUTPUT_TRUNCATED_MARKER));
}

/// Concurrent supervisors must overlap: the process-wide signal-handler
/// mutex used to be held for each child's entire lifetime, so two `sleep 1`
/// waits from two threads took ~2 s instead of ~1 s.
#[cfg(unix)]
#[test]
fn concurrent_supervised_sleeps_overlap() {
    let started = Instant::now();
    std::thread::scope(|scope| {
        let first = scope.spawn(|| supervise_sleep("1"));
        let second = scope.spawn(|| supervise_sleep("1"));
        let first = first
            .join()
            .expect("first supervisor thread")
            .expect("first wait");
        let second = second
            .join()
            .expect("second supervisor thread")
            .expect("second wait");
        assert!(first.exit_success, "first sleep should exit 0");
        assert!(second.exit_success, "second sleep should exit 0");
    });
    assert!(
        started.elapsed() < Duration::from_millis(1_500),
        "concurrent sleep 1 children serialized: {:?}",
        started.elapsed()
    );
}

#[cfg(unix)]
fn supervise_sleep(seconds: &str) -> Result<WaitResult, orbit_common::OrbitError> {
    let req = ExecRequest {
        program: "/bin/sleep".to_string(),
        args: vec![seconds.to_string()],
        current_dir: None,
        timeout_ms: Some(5_000),
        stdin_mode: StdinMode::Null,
        environment_mode: EnvironmentMode::Inherit,
        debug: false,
    };
    let child = crate::process::spawn(&req).expect("spawn child");
    wait_with_timeout_and_output_limit(child, Some(5_000), false, None, 64)
}
