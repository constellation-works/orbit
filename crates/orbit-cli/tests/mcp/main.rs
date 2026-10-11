//! The production MCP entry point (`orbit mcp serve`) and MCP client setup
//! through the built `orbit` binary.
//!
//! One integration-test binary per area keeps link cost down; add a module
//! here rather than a new top-level `tests/*.rs` file
//! (`docs/design-patterns/test_strategy.md`).

// Integration fixtures unwrap setup invariants.
#![allow(clippy::expect_used, clippy::unwrap_used)]

orbit_common::isolate_test_process!();

#[path = "../support/child_guard.rs"]
mod child_guard;
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[path = "../support/generation_fixture.rs"]
mod generation_fixture;
#[cfg(unix)]
#[path = "../support/git_authority.rs"]
mod git_authority;
#[path = "../support/git_repo.rs"]
mod git_repo;
#[path = "../support/isolated_test.rs"]
mod isolated_test;

mod mcp_roundtrip;
mod mcp_setup_preservation;
mod mcp_setup_root;

/// Re-execute the real setup, status/push and linked-worktree fixtures with
/// repository locators aimed at a disposable sentinel. Re-executed children
/// keep the deliberate locators: each fixture Git command must scrub them.
#[cfg(unix)]
#[test]
fn mcp_git_fixtures_preserve_inherited_authority_decoy() {
    git_authority::assert_fixtures_preserve_decoy(&[
        "mcp_roundtrip::fresh_workspace_init_mcp_explains_local_ship_and_allows_pr_worktree_setup",
        "mcp_roundtrip::managed_mcp_config_routes_a_linked_worktree_by_its_logical_workspace_id",
        "mcp_setup_root::mcp_init_binds_an_external_root_checkout_and_remove_reverses_it",
    ]);
}
