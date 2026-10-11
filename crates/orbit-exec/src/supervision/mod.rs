mod cleanup;
#[cfg(unix)]
mod signal;
mod tee;
mod wait;

pub(crate) use cleanup::{terminate_process_group, termination_signal};
pub(crate) use wait::{
    SupervisedChild, WaitResult, wait_with_cancellation, wait_with_optional_timeout,
    wait_with_spawn_cancellation, wait_with_stdout_relay,
};

#[cfg(test)]
mod tests;

/// Prefix of the supervisor note when output capture stops a process.
pub const OUTPUT_CAPTURE_LIMIT_NOTE: &str = "process output capture limit exceeded on ";
/// Prefix of the supervisor note when an incoming signal stops a process.
pub const INTERRUPTED_SIGNAL_NOTE: &str = "process interrupted by signal ";
/// Prefix of the supervisor note when a child terminates without an exit code.
pub const TERMINATION_STATUS_NOTE: &str = "process terminated with ";
