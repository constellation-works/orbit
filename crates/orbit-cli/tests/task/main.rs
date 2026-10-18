//! Task, friction, auto-task and audit records through the built `orbit`
//! binary, including context-selector validation and shared-root isolation.
//!
//! One integration-test binary per area keeps link cost down; add a module
//! here rather than a new top-level `tests/*.rs` file
//! (`docs/design-patterns/test_strategy.md`).

// Integration fixtures unwrap setup invariants.
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "../support/fixture_crew.rs"]
mod fixture_crew;
#[path = "../support/git_repo.rs"]
mod git_repo;
#[path = "../support/isolated_cli_fixture.rs"]
mod isolated_cli_fixture;
#[path = "../support/isolated_test.rs"]
mod isolated_test;
#[path = "../support/task_workspace.rs"]
mod task_workspace;

mod audit_cli;
mod auto_task_lifecycle_cli;
mod auto_task_listing_cli;
mod auto_task_settings_cli;
mod claimed_delivery_recovery;
mod claimed_operator_comment;
mod codebase_cli;
mod config_show_task_reads;
mod context_selector_external_root;
mod context_selector_worktree;
mod crew_effort_admission;
mod delivery_remote_source;
mod friction_lifecycle_cli;
mod live_run_status;
mod local_read_projections_cli;
mod pilot_comments;
mod reconcile_review;
mod rescue_close_cli;
mod review_after_landing_cli;
mod review_crew_pool_cli;
mod review_reset_claimed;
mod shared_root_task_isolation;
mod task_admin_cli;
mod task_eligible;
mod task_list;
mod task_publication;
mod task_recheck_blocked_cli;
mod task_registry_recovery;
mod task_tags;
mod task_trimmed_surface;
mod write_preflight;
