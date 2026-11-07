//! The native manager runner must not wait on a wedged `launchctl`/`systemctl`.

use std::time::{Duration, Instant};

use orbit_common::OrbitError;

use super::super::manager::{ManagerCommand, NativeClockCommandRunner};

#[test]
fn hanging_manager_probe_times_out_naming_the_command() {
    let command = ManagerCommand {
        program: "sleep",
        args: vec!["600".to_string()],
    };
    let started = Instant::now();
    let error = NativeClockCommandRunner::execute(&command, Duration::from_millis(300))
        .expect_err("a probe that never exits must fail");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the probe must return near its bound, took {:?}",
        started.elapsed()
    );
    match &error {
        OrbitError::ProcessTimeout { timeout_ms, detail } => {
            assert_eq!(*timeout_ms, 300);
            assert!(
                detail.contains("sleep 600"),
                "the timeout must name the command: {detail}"
            );
        }
        other => panic!("expected ProcessTimeout, got {other:?}"),
    }
}

#[test]
fn missing_manager_program_names_the_command() {
    let command = ManagerCommand {
        program: "orbit-no-such-manager",
        args: vec!["list".to_string()],
    };
    let error = NativeClockCommandRunner::execute(&command, Duration::from_secs(5))
        .expect_err("a missing program cannot run");
    assert!(
        error.to_string().contains("orbit-no-such-manager list"),
        "the spawn failure must name the command: {error}"
    );
}
