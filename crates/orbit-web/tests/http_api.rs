//! HTTP contracts through the public server entry point, in disposable child processes.
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "support/git.rs"]
mod git_test_support;

#[path = "http_api/agents.rs"]
mod agents;
#[path = "http_api/agents_implementers.rs"]
mod agents_implementers;
#[path = "http_api/agents_orchestrators.rs"]
mod agents_orchestrators;
#[path = "http_api/agents_pilots.rs"]
mod agents_pilots;
#[path = "http_api/agents_reviewers.rs"]
mod agents_reviewers;
#[path = "http_api/aggregate_pages.rs"]
mod aggregate_pages;
#[path = "http_api/audit.rs"]
mod audit;
#[path = "http_api/audit_contracts.rs"]
mod audit_contracts;
#[path = "http_api/auto_tasks.rs"]
mod auto_tasks;
#[path = "http_api/automation.rs"]
mod automation;
#[path = "http_api/codebase.rs"]
mod codebase;
#[path = "http_api/compression.rs"]
mod compression;
#[path = "http_api/crews.rs"]
mod crews;
#[path = "http_api/dashboard_assets.rs"]
mod dashboard_assets;
#[path = "http_api/diagnostics_errors.rs"]
mod diagnostics_errors;
#[path = "http_api/distributed_contracts.rs"]
mod distributed_contracts;
#[path = "http_api/doctor.rs"]
mod doctor;
#[path = "http_api/embedded.rs"]
mod embedded;
#[path = "http_api/friction_writes.rs"]
mod friction_writes;
#[path = "http_api/guards.rs"]
mod guards;
#[path = "http_api/host.rs"]
mod host;
#[path = "http_api/host_forward.rs"]
#[cfg(unix)]
mod host_forward;
#[path = "http_api/job_run_lists.rs"]
mod job_run_lists;
#[path = "http_api/log.rs"]
mod log;
#[path = "http_api/log_render.rs"]
mod log_render;
#[path = "http_api/metrics_trends.rs"]
mod metrics_trends;
#[path = "http_api/pagination.rs"]
mod pagination;
#[path = "http_api/plugins.rs"]
#[cfg(unix)]
mod plugins;
#[path = "http_api/projections.rs"]
mod projections;
#[path = "http_api/provider_auth.rs"]
mod provider_auth;
#[path = "http_api/reliability.rs"]
mod reliability;
#[path = "http_api/resource_profiles.rs"]
mod resource_profiles;
#[path = "http_api/runs.rs"]
mod runs;
#[path = "http_api/support.rs"]
mod support;
#[path = "http_api/task_contracts.rs"]
mod task_contracts;
#[path = "http_api/task_create.rs"]
mod task_create;
#[path = "http_api/task_pilot.rs"]
mod task_pilot;
#[path = "http_api/task_writes.rs"]
mod task_writes;
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
