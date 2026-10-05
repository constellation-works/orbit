#![allow(clippy::expect_used, clippy::unwrap_used)]

mod dashboard_assets;
mod log_format;
mod serve;
#[cfg(unix)]
mod ssh_tunnel;
