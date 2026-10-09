//! Task, friction, auto-task and audit records through the built `orbit`
//! binary, including context-selector validation and shared-root isolation.
//!
//! One integration-test binary per area keeps link cost down; add a module
//! here rather than a new top-level `tests/*.rs` file
//! (`docs/design-patterns/test_strategy.md`).

// Integration fixtures unwrap setup invariants.
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "../support/git_authority.rs"]
mod git_authority;
#[path = "../support/fixture_crew.rs"]
mod fixture_crew;
#[path = "../support/git_repo.rs"]
mod git_repo;
#[path = "../support/isolated_cli_fixture.rs"]
mod isolated_cli_fixture;

mod audit_cli;
mod auto_task_lifecycle_cli;
mod auto_task_settings_cli;
mod claimed_delivery_recovery;
mod context_selector_external_root;
mod context_selector_worktree;
mod crew_effort_admission;
mod delivery_remote_source;
mod friction_lifecycle_cli;
mod local_read_projections_cli;
mod pilot_comments;
mod reconcile_review;
mod rescue_close_cli;
mod review_after_landing_cli;
mod shared_root_task_isolation;
mod task_admin_cli;
mod task_eligible;
mod task_list;
mod task_publication;
mod task_recheck_blocked_cli;
mod task_tags;
mod task_trimmed_surface;

#[test]
fn task_git_fixtures_preserve_inherited_authority_decoy() {
    git_authority::assert_fixtures_preserve_decoy(&[
        "task_publication::operator_workflow_is_network_free_labelled_and_fail_closed",
        "shared_root_task_isolation::shared_explicit_root_keeps_task_bundles_isolated_by_selected_workspace",
        "context_selector_external_root::linked_worktree_of_an_external_root_checkout_can_declare_its_own_file",
        "context_selector_worktree::linked_worktree_caller_can_declare_a_file_that_exists_only_there",
        "auto_task_lifecycle_cli::fixture_git_commits_and_refs_stay_in_the_fixture",
        "auto_task_lifecycle_cli::seeded_auto_task_defaults_are_inert_portable_and_name_only_callable_tools",
        "audit_cli::audit_cli_round_trips_real_mutation_filters_stats_and_export",
    ]);
}
