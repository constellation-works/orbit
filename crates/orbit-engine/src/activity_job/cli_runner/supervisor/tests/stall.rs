//! A silent provider is ended once its stall window elapses. A provider that
//! burns CPU or keeps printing is left to finish, and a disabled window
//! leaves only the wall-clock deadline.

use std::cell::Cell;
use std::time::Duration;

use super::super::{SpawnTraceContext, SpawnWithTimeoutRequest, spawn_with_timeout};
use super::test_support::{process_is_live, spawn_test_request};
use crate::activity_job::cli_runner::tests::test_support::sh_args;

const WINDOW: Duration = Duration::from_millis(500);

#[derive(Debug)]
struct Ran {
    stalled: bool,
    timed_out: bool,
    exit_code: Option<i32>,
    duration: Duration,
    pid: u32,
}

fn run(program: &str, args: &[String], stall: Option<Duration>, timeout: Duration) -> Ran {
    let pid = Cell::new(0u32);
    let on_spawn = |spawned: u32| pid.set(spawned);
    let (_stdout, _stderr, exit_code, duration, timed_out, stalled) =
        spawn_with_timeout(SpawnWithTimeoutRequest {
            stall_window: stall,
            on_spawn: Some(&on_spawn),
            ..spawn_test_request(
                program,
                args,
                None,
                timeout,
                SpawnTraceContext {
                    provider: "fixture",
                    job_run_id: "stall-window",
                    task_id: None,
                    cwd: None,
                },
            )
        })
        .expect("spawn the stall fixture");
    Ran {
        stalled,
        timed_out,
        exit_code,
        duration,
        pid: pid.get(),
    }
}

/// Silent sleep is killed after the window. A child that burns CPU without
/// printing, and a child that prints on a shorter period, both run to exit 0.
/// With the watch disabled, the same sleeper ends only on the wall clock.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn silent_sleep_stalls_while_cpu_and_output_keep_the_run_alive() {
    let wall = Duration::from_secs(4);
    let sleep_args = vec!["30".to_string()];

    let silent = run("/bin/sleep", &sleep_args, Some(WINDOW), wall);
    assert!(silent.stalled, "silent sleep should stall: {silent:?}");
    assert!(
        !silent.timed_out,
        "stall is not a wall-clock timeout: {silent:?}"
    );
    assert!(silent.exit_code.is_none(), "{silent:?}");
    assert!(silent.duration >= WINDOW, "{silent:?}");
    assert!(
        silent.duration < Duration::from_secs(3),
        "stall should return before the wall clock: {silent:?}"
    );
    assert!(
        !process_is_live(silent.pid),
        "stalled sleeper {} should be gone",
        silent.pid
    );

    // The shell itself sleeps. Its child is what consumes CPU, so a sampler
    // that reads only the parent's own time would stall this run.
    let burn = sh_args(
        "burn=$(command -v yes) || exit 2; \"$burn\" >/dev/null & child=$!; \
         sleep 1.5; kill \"$child\" 2>/dev/null; wait \"$child\" 2>/dev/null; exit 0",
    );
    let burning = run("/bin/sh", &burn, Some(WINDOW), wall);
    assert!(
        !burning.stalled,
        "cpu without output should survive: {burning:?}"
    );
    assert!(!burning.timed_out, "{burning:?}");
    assert_eq!(burning.exit_code, Some(0), "{burning:?}");
    assert!(
        burning.duration >= Duration::from_millis(1200),
        "{burning:?}"
    );

    let print = sh_args(
        "i=0; while [ \"$i\" -lt 16 ]; do printf 'tick\\n'; i=$((i + 1)); sleep 0.1; done; exit 0",
    );
    let printing = run("/bin/sh", &print, Some(WINDOW), wall);
    assert!(
        !printing.stalled,
        "periodic output should survive: {printing:?}"
    );
    assert!(!printing.timed_out, "{printing:?}");
    assert_eq!(printing.exit_code, Some(0), "{printing:?}");
    assert!(
        printing.duration >= Duration::from_millis(1200),
        "{printing:?}"
    );

    let disabled = run("/bin/sleep", &sleep_args, None, Duration::from_millis(400));
    assert!(
        !disabled.stalled,
        "a disabled window does not stall: {disabled:?}"
    );
    assert!(
        disabled.timed_out,
        "the wall clock still ends it: {disabled:?}"
    );
    assert!(disabled.exit_code.is_none(), "{disabled:?}");
    assert!(disabled.duration < Duration::from_secs(2), "{disabled:?}");
}
