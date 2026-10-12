use std::process::Child;
use std::time::Duration;

use orbit_common::OrbitError;
use orbit_common::process::OwnedChild;

pub(super) const TERMINATION_GRACE_PERIOD: Duration = Duration::from_secs(5);

// Transitional adapter for build phases, which still hold a bare Child.
pub(crate) fn terminate_process_group(
    child: &mut Child,
    signal: i32,
    poll_interval: Duration,
) -> Result<(), OrbitError> {
    OwnedChild::terminate_spawned(child, signal, TERMINATION_GRACE_PERIOD, poll_interval)
        .map_err(|error| OrbitError::Execution(format!("failed waiting for process: {error}")))
}

#[cfg(unix)]
pub(crate) fn termination_signal() -> i32 {
    libc::SIGTERM
}

#[cfg(not(unix))]
pub(crate) fn termination_signal() -> i32 {
    15
}

#[cfg(all(test, unix))]
pub(super) fn kill_process_group(pid: u32) {
    let _ = OwnedChild::signal_spawned_group(pid, libc::SIGKILL);
}
