//! The process-group liveness probe at its public boundary: pid 0 names the
//! caller's own group to `killpg`, so the probe must never report it as alive.
//! Native Linux scanning must also distinguish an unreaped zombie-only group.
#![cfg(unix)]
#![allow(missing_docs, clippy::expect_used, clippy::unwrap_used)]

orbit_common::isolate_test_process!();

use orbit_common::process::identity::{KernelLiveness, probe_process_group_liveness};

#[test]
fn pid_zero_is_not_probed_as_the_callers_group() {
    // Safety: `getpgrp` only reads the calling process's group id.
    let own_group = unsafe { libc::getpgrp() };
    assert_eq!(
        probe_process_group_liveness(own_group),
        KernelLiveness::Alive,
        "the test process must be a live member of its own group, or pid 0 proves nothing"
    );
    assert_eq!(
        probe_process_group_liveness(0),
        KernelLiveness::Exited,
        "pid 0 must not probe as the caller's group"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn unreaped_linux_zombie_group_is_exited_despite_signal_presence() {
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use orbit_common::process::identity::linux_process_state;

    struct ReapingChild(Child);
    impl Drop for ReapingChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    let mut child = ReapingChild(
        Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .expect("spawn child in its own group"),
    );
    let pid = child.0.id();
    let pgid = pid as libc::pid_t;
    let deadline = Instant::now() + Duration::from_secs(3);
    while linux_process_state(pid) != Some(('Z', pgid)) {
        assert!(
            Instant::now() < deadline,
            "child must become an unreaped zombie"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    // Safety: signal 0 checks presence without signalling any process.
    assert_eq!(
        unsafe { libc::kill(-pgid, 0) },
        0,
        "unreaped group still exists"
    );
    assert_eq!(probe_process_group_liveness(pgid), KernelLiveness::Exited);
    child.0.wait().expect("reap zombie");
}
