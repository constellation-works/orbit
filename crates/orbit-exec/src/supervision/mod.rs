mod cleanup;
#[cfg(unix)]
mod signal;
mod tee;
mod wait;

pub(crate) use wait::{wait_with_cancellation, wait_with_optional_timeout};

#[cfg(test)]
mod tests;
