//! Workspace and machine setup through the built `orbit` binary: init,
//! registration and selection, sync, routines, sweeps and worktree GC routing.
//! `agent_rules`, `doctor_runtime` and `layout_version` call the exported
//! `orbit_cmd` guide injector, doctor and migration entry points directly.
//!
//! One integration-test binary per area keeps link cost down; add a module
//! here rather than a new top-level `tests/*.rs` file
//! (`docs/design-patterns/test_strategy.md`).

// Integration fixtures unwrap setup invariants.
#![allow(clippy::expect_used, clippy::unwrap_used)]

// ChildGuard has a portable kill/wait fallback; its Unix signals and process
// groups are guarded inside the shared module, as in the process binary.
#[path = "../support/child_guard.rs"]
mod child_guard;
#[path = "../support/fixture_crew.rs"]
mod fixture_crew;
#[path = "../support/git_repo.rs"]
mod git_repo;
#[path = "../support/isolated_test.rs"]
mod isolated_test;
#[path = "../support/routine_runs.rs"]
mod routine_runs;
#[cfg(unix)]
#[path = "../support/workflow_gate.rs"]
mod workflow_gate;

mod agent_rules;
mod ambient_authority_isolation;
mod config_set_root;
#[cfg(unix)]
mod doctor_confirm;
mod doctor_runtime;
mod generation_root;
mod git_fixture_isolation;
#[cfg(unix)]
mod host_dashboard;
#[cfg(unix)]
mod host_fleet;
#[cfg(unix)]
mod host_registry;
#[cfg(unix)]
mod host_routing;
mod init_force_identity;
mod init_interactive_stdin;
#[cfg(all(target_os = "linux", target_endian = "little"))]
mod init_linux_sandbox;
mod init_minted_prefix;
mod init_workspace_seed;
mod layout_version;
mod parked_worktree_gc;
mod replica_routines;
mod routine_root;
mod routine_state_seed;
mod run_profiles;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod scratch_gc;
mod ship_sweep_root;
#[cfg(unix)]
mod store_gc;
mod sweep_root;
mod sweep_workspace;
#[cfg(unix)]
mod teardown_retry;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod tmp_gc;
mod workspace_selector;
mod workspace_ship_mode;
mod workspace_source_remote;
mod workspace_sync;
mod worktree_gc_routing;
