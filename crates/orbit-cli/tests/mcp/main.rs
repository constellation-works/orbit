//! The production MCP entry point (`orbit mcp serve`) and MCP client setup
//! through the built `orbit` binary.
//!
//! One integration-test binary per area keeps link cost down; add a module
//! here rather than a new top-level `tests/*.rs` file
//! (`docs/design-patterns/test_strategy.md`).

// Integration fixtures unwrap setup invariants.
#![allow(clippy::expect_used, clippy::unwrap_used)]

orbit_common::isolate_test_process!();

#[path = "../support/bound_server.rs"]
mod bound_server;
#[path = "../support/child_guard.rs"]
mod child_guard;
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[path = "../support/generation_fixture.rs"]
mod generation_fixture;
#[path = "../support/git_repo.rs"]
mod git_repo;
#[path = "../support/isolated_test.rs"]
mod isolated_test;

mod mcp_roundtrip;
mod mcp_setup_preservation;
mod mcp_setup_root;
