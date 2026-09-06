//! Test-only allowlist: the original tests under orbit-cli passed the same lints via
//! the crate-level test harness configuration; duplicated here for the extracted crate.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod test_support;

mod audit;
mod auto_tasks;
mod denials;
mod diagnostics;
mod frictions;
mod handlers;
mod incidents;
mod log;
mod metrics;
mod reliability;
mod routines;
mod runs;
mod scoreboard;
mod search;
mod tasks;
mod workspaces;

mod task_listing;

mod task_response_bench;
