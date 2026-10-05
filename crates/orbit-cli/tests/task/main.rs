//! Task, friction, auto-task and audit records through the built `orbit`
//! binary, including context-selector validation and shared-root isolation.
//!
//! One integration-test binary per area keeps link cost down; add a module
//! here rather than a new top-level `tests/*.rs` file
//! (`docs/design-patterns/test_strategy.md`).

// Integration fixtures unwrap setup invariants.
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "../support/git_repo.rs"]
mod git_repo;
#[path = "../support/isolated_cli_fixture.rs"]
mod isolated_cli_fixture;

mod audit_cli;
mod auto_task_lifecycle_cli;
mod context_selector_external_root;
mod context_selector_worktree;
mod crew_effort_admission;
mod delivery_remote_source;
mod friction_lifecycle_cli;
mod local_read_projections_cli;
mod review_after_landing_cli;
mod shared_root_task_isolation;
mod task_admin_cli;
mod task_eligible;
mod task_list;
mod task_publication;
mod task_recheck_blocked_cli;
mod task_tags;
mod task_trimmed_surface;
