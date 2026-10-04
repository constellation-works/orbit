//! Long-lived `orbit` processes: detached job workers, run observation,
//! signal handling, `web serve` and self-update.
//!
//! One integration-test binary per area keeps link cost down; add a module
//! here rather than a new top-level `tests/*.rs` file
//! (`docs/design-patterns/test_strategy.md`).

// Integration fixtures unwrap setup invariants.
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "../support/fixture_crew.rs"]
mod fixture_crew;

mod job_resume_detached;
mod run_observation;
mod supervised_parent_signal;
mod update;
mod web_serve_handover;
mod web_serve_root;
mod web_serve_shutdown;
mod worktree_resolution;
