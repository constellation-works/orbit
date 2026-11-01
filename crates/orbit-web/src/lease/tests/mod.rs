#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "listener and intake fixtures panic when a socket, token or permission setup fails"
)]

#[cfg(unix)]
mod intake;
#[cfg(unix)]
mod listener;
