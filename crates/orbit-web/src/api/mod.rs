//! JSON HTTP handlers for the dashboard.
//!
//! Each handler delegates to the same `*_to_json` helpers used by the CLI's
//! `--json` paths so the wire format stays in lockstep with the CLI.

// Test-only allowlist (mirrors the original placement under orbit-cli): the many
// `.expect` / `.unwrap` calls in the in-file integration tests and the included
// `*_tests` modules are the documented exception for test harness code.
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]

mod audit;
mod auto_tasks;
mod automation;
mod config;
mod crews;
mod denials;
mod diagnostics;
mod distributed;
mod frictions;
mod helpers;
mod incidents;
mod jobs;
mod log;
mod metrics;
mod origin;
mod pagination;
mod plugins;
mod reliability;
mod routes;
mod routines;
mod runs;
mod scoreboard;
mod search;
mod tasks;
mod workspaces;

use helpers::*;
pub(crate) use origin::{nosniff_json_responses, require_localhost_origin};
pub(super) use routes::{request_shutdown, router};

#[cfg(test)]
mod tests;
