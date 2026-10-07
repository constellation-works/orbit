//! Workspace and machine setup through the built `orbit` binary: init,
//! registration and selection, sync, routines, sweeps and worktree GC routing.
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

mod ambient_authority_isolation;
mod generation_root;
mod init_interactive_stdin;
#[cfg(all(target_os = "linux", target_endian = "little"))]
mod init_linux_sandbox;
mod init_minted_prefix;
mod replica_routines;
mod routine_root;
mod routine_state_seed;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod scratch_gc;
mod ship_sweep_root;
mod sweep_root;
mod sweep_workspace;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod tmp_gc;
mod workspace_selector;
mod workspace_source_remote;
mod workspace_sync;
mod worktree_gc_routing;
