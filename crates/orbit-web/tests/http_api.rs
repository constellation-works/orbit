//! HTTP contracts through the public server entry point, in disposable child processes.
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "http_api/guards.rs"]
mod guards;
#[path = "http_api/host.rs"]
mod host;
#[path = "http_api/log.rs"]
mod log;
#[path = "http_api/plugins.rs"]
#[cfg(unix)]
mod plugins;
#[path = "http_api/projections.rs"]
mod projections;
#[path = "http_api/support.rs"]
mod support;
#[path = "http_api/workflows.rs"]
mod workflows;

/// Re-executed only by the fixture launcher; never a second implementation of the router.
#[test]
#[ignore = "server child entry point"]
fn server_child() {
    support::serve_fixture();
}
