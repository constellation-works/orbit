use std::time::Duration;

#[cfg(all(test, unix))]
use orbit_common::process::OwnedChild;

pub(crate) const TERMINATION_GRACE_PERIOD: Duration = Duration::from_secs(5);

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
