//! Tools, plugins and skills through the built `orbit` binary: listing,
//! lifecycle, `tool run` audit, plugin command groups, secrets and the broker
//! sandbox.
//!
//! One integration-test binary per area keeps link cost down; add a module
//! here rather than a new top-level `tests/*.rs` file
//! (`docs/design-patterns/test_strategy.md`).

// Integration fixtures unwrap setup invariants.
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[cfg(unix)]
#[path = "../support/child_guard.rs"]
mod child_guard;
#[path = "../support/fixture_crew.rs"]
mod fixture_crew;
#[path = "../support/git_authority.rs"]
mod git_authority;
#[path = "../support/git_repo.rs"]
mod git_repo;
#[path = "../support/isolated_cli_fixture.rs"]
mod isolated_cli_fixture;
#[path = "../support/tool_input_hints.rs"]
mod tool_input_hint_cases;

mod claimed_review_bridge_sandbox;
mod github_broker_sandbox;
mod github_capability_preflight;
mod plugin_broker_sandbox;
mod plugin_child_cli_surface;
mod plugin_cli_group;
mod plugin_hook_workspace_probe;
mod plugin_secrets;
mod proc_spawn_managed;
mod skill_lifecycle_cli;
mod tool_input_hints;
mod tool_lifecycle_cli;
mod tool_list;
mod tool_run_audit;

#[test]
fn tool_git_fixtures_preserve_inherited_authority_decoy() {
    git_authority::assert_fixtures_preserve_decoy(&[
        "proc_spawn_managed::managed_pilot_proc_spawn_inspects_the_pinned_checkout_not_the_primary",
    ]);
}
