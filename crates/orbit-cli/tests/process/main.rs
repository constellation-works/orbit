//! Long-lived `orbit` processes: detached job workers, run observation,
//! signal handling, `web serve` and self-update.
//!
//! One integration-test binary per area keeps link cost down; add a module
//! here rather than a new top-level `tests/*.rs` file
//! (`docs/design-patterns/test_strategy.md`).

// Integration fixtures unwrap setup invariants.
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "../support/bound_server.rs"]
mod bound_server;
#[path = "../support/child_guard.rs"]
mod child_guard;
#[path = "../support/fixture_crew.rs"]
mod fixture_crew;
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[path = "../support/generation_fixture.rs"]
mod generation_fixture;
#[path = "../support/git_repo.rs"]
mod git_repo;
#[cfg(unix)]
#[path = "../support/isolated_test.rs"]
mod isolated_test;
#[cfg(unix)]
#[path = "../support/workflow_gate.rs"]
mod workflow_gate;

mod env_pass_warning;
mod generation_participation;
mod job_resume_detached;
mod run_observation;
mod server_readiness;
mod supervised_parent_signal;
mod update;
#[cfg(unix)]
mod web_fixture_io;
mod web_serve_handover;
mod web_serve_root;
mod web_serve_shutdown;
mod worktree_resolution;
