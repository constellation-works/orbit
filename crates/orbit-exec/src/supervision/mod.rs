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
