use std::time::Duration;

pub(crate) const TERMINATION_GRACE_PERIOD: Duration = Duration::from_secs(5);

#[cfg(unix)]
pub(crate) fn termination_signal() -> i32 {
    libc::SIGTERM
}

#[cfg(not(unix))]
pub(crate) fn termination_signal() -> i32 {
    15
}
