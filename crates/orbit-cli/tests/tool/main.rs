//! Tools, plugins and skills through the built `orbit` binary: listing,
//! lifecycle, `tool run` audit, plugin command groups, secrets and the broker
//! sandbox.
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

mod github_broker_sandbox;
mod github_capability_preflight;
mod plugin_broker_sandbox;
mod plugin_child_cli_surface;
mod plugin_cli_group;
mod plugin_hook_workspace_probe;
mod plugin_secrets;
mod proc_spawn_managed;
mod skill_lifecycle_cli;
mod tool_lifecycle_cli;
mod tool_list;
mod tool_run_audit;
