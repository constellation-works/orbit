//! Caller policy checks stop a child through the shared supervisor.

#![cfg(unix)]
#![allow(clippy::expect_used)]

use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::Duration;

use orbit_common::process::supervisor::{
    CaptureSink, Periodic, ProcessStopCause, Stdin, Stream, SupervisedChild, Supervision,
    Termination,
};

#[test]
fn periodic_checks_continue_until_the_caller_requests_a_stop() {
    let child = Command::new("/bin/sh")
        .args(["-c", "printf ready; exec sleep 30"])
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    let cause = ProcessStopCause::Requested {
        reason: "fixture policy",
    };
    let mut checks = 0;
    let mut check = || {
        checks += 1;
        (checks == 3).then_some(cause)
    };
    let outcome = SupervisedChild::adopt(child)
        .supervise(Supervision {
            stdin: Stdin::Null,
            stdout: Stream::Sink(Box::new(CaptureSink::new("stdout", 1024))),
            stderr: Stream::Sink(Box::new(CaptureSink::new("stderr", 1024))),
            timeout: Some(Duration::from_secs(10)),
            clock: None,
            extendable_deadline: None,
            termination: Termination::Grace(Duration::from_millis(100)),
            cancelled: None,
            intercept_signals: false,
            drain_budget: Duration::from_secs(1),
            cap_drain_at_deadline: false,
            require_eof: false,
            stopped_descendants: None,
            on_stopped_descendant: None,
            on_spawn: None,
            every: Some(Periodic {
                interval: Duration::from_millis(50),
                run: &mut check,
            }),
        })
        .expect("supervise");
    assert_eq!(checks, 3);
    assert_eq!(outcome.stop_causes, vec![cause]);
    assert!(!outcome.success);
    assert!(!outcome.timed_out);
    assert_eq!(outcome.stdout, b"ready");
}
