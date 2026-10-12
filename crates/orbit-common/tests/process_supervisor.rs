//! Supervision options composed through the public child-process boundary.
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[cfg(unix)]
#[test]
fn extendable_deadline_is_refreshed_before_expiry_and_periodic_checks_can_stop() {
    use std::cell::Cell;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use orbit_common::process::supervisor::{
        Periodic, ProcessStopCause, Stdin, Stream, SupervisedChild, Supervision, Termination,
    };

    let child = Command::new("/bin/sleep")
        .arg("30")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .expect("spawn process-group leader");
    let pid = child.id();
    let spawned = Cell::new(None);
    let deadline_reads = Cell::new(0);
    let periodic_checks = Cell::new(0);
    let frozen = Instant::now();
    // The first reading is already expired. The fresh reading credits queued
    // time, so the supervisor must keep running until the periodic check stops
    // it, even though its fixed timeout is zero.
    let extendable = || {
        let reads = deadline_reads.get();
        deadline_reads.set(reads + 1);
        if reads == 0 {
            frozen
        } else {
            Instant::now() + Duration::from_secs(30)
        }
    };
    let mut check = || {
        let checks = periodic_checks.get() + 1;
        periodic_checks.set(checks);
        (checks == 2).then_some(ProcessStopCause::Periodic)
    };
    let outcome = SupervisedChild::adopt(child)
        .supervise(Supervision {
            stdin: Stdin::Null,
            stdout: Stream::Redirected,
            stderr: Stream::Redirected,
            timeout: Some(Duration::ZERO),
            clock: Some(&|| frozen),
            extendable_deadline: Some(&extendable),
            termination: Termination::KillNow,
            cancelled: None,
            intercept_signals: false,
            drain_budget: Duration::ZERO,
            stopped_descendants: None,
            on_stopped_descendant: None,
            on_spawn: Some(&|pid| spawned.set(Some(pid))),
            every: Some(Periodic {
                interval: Duration::from_millis(10),
                run: &mut check,
            }),
        })
        .expect("supervise combined deadline and periodic check");

    assert_eq!(
        spawned.get(),
        Some(pid),
        "spawn callback retains child identity"
    );
    assert!(
        deadline_reads.get() >= 2,
        "expiry requires a fresh extendable deadline reading"
    );
    assert_eq!(
        periodic_checks.get(),
        2,
        "non-stopping progress checks continue until a stop cause"
    );
    assert!(
        !outcome.success && !outcome.timed_out,
        "extendable deadline takes precedence over the fixed timeout: {outcome:?}"
    );
    assert_eq!(outcome.stop_cause(), Some(ProcessStopCause::Periodic));
}
