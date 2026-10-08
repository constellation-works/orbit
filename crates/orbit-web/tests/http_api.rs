//! HTTP contracts through the public server entry point, in disposable child processes.
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "http_api/audit.rs"]
mod audit;
#[path = "http_api/auto_tasks.rs"]
mod auto_tasks;
#[path = "http_api/automation.rs"]
mod automation;
#[path = "http_api/diagnostics_errors.rs"]
mod diagnostics_errors;
#[path = "http_api/guards.rs"]
mod guards;
#[path = "http_api/host.rs"]
mod host;
#[path = "http_api/log.rs"]
mod log;
#[path = "http_api/pagination.rs"]
mod pagination;
#[path = "http_api/plugins.rs"]
#[cfg(unix)]
mod plugins;
#[path = "http_api/projections.rs"]
mod projections;
#[path = "http_api/runs.rs"]
mod runs;
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

/// Executes the submitted job in a separate process against the fixture store.
#[test]
#[ignore = "replay worker child entry point"]
fn replay_worker_child() {
    support::execute_replay_worker();
}
