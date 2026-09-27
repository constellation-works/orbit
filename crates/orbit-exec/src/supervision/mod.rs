mod cleanup;
#[cfg(unix)]
mod signal;
mod tee;
mod wait;

pub(crate) use wait::{wait_with_cancellation, wait_with_optional_timeout, wait_with_stdout_relay};

#[cfg(test)]
mod tests;
