//! JSON HTTP handlers for the dashboard.
//!
//! Each handler delegates to the same `*_to_json` helpers used by the CLI's
//! `--json` paths so the wire format stays in lockstep with the CLI.

mod agents;
mod audit;
mod auto_tasks;
mod automation;
mod codebase;
mod config;
mod cost;
mod crews;
mod dashboard_chrome;
mod decisions;
mod denials;
mod diagnostics;
mod distributed;
mod doctor;
mod events;
mod forward;
mod frictions;
mod helpers;
mod host;
mod incidents;
mod jobs;
pub(crate) mod lease_notices;
pub(crate) mod leases;
mod log;
mod metrics;
mod notices;
mod operation_audit;
mod origin;
mod pagination;
mod plugins;
mod reliability;
mod remote_access;
mod residents;
mod resource_profiles;
mod route;
mod routine_breaker;
pub(crate) mod routines;
mod runs;
mod schedule_edits;
mod scripts;
mod search;
mod task_search;
mod tasks;
mod workspaces;

use helpers::*;
pub(crate) use origin::{nosniff_json_responses, require_localhost_host, require_localhost_origin};
pub(super) use route::{request_shutdown, router};

#[cfg(test)]
pub(crate) mod tests;
