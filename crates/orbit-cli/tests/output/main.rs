//! How the `orbit` binary renders: `--help` and list goldens, `--json` and
//! `--format` contracts, table layout and error reporting.
//!
//! One integration-test binary per area keeps link cost down; add a module
//! here rather than a new top-level `tests/*.rs` file
//! (`docs/design-patterns/test_strategy.md`).

// Integration fixtures unwrap setup invariants.
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "../support/git_repo.rs"]
mod git_repo;

#[path = "../support/output.rs"]
mod output;

mod doctor;
mod error_output;
mod global_json;
mod help_examples;
mod help_goldens;
mod help_skips_log_io;
mod json_output_stability;
#[cfg(unix)]
mod log_tail;
mod machine_readable_confirmations;
mod output_goldens;
mod table_rendering;
