//! The process-group liveness probe at its public boundary: pid 0 names the
//! caller's own group to `killpg`, so the probe must never report it as alive.
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
