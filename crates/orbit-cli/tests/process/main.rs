//! Long-lived `orbit` processes: detached job workers, run observation,
//! signal handling, `web serve` and self-update.
//!
//! One integration-test binary per area keeps link cost down; add a module
//! here rather than a new top-level `tests/*.rs` file
//! (`docs/design-patterns/test_strategy.md`).

// Integration fixtures unwrap setup invariants.
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "../support/child_guard.rs"]
mod child_guard;
#[path = "../support/fixture_crew.rs"]
mod fixture_crew;
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[path = "../support/generation_fixture.rs"]
mod generation_fixture;
#[cfg(unix)]
#[path = "../support/git_authority.rs"]
mod git_authority;
#[path = "../support/git_repo.rs"]
mod git_repo;

mod env_pass_warning;
mod job_resume_detached;
mod run_observation;
mod supervised_parent_signal;
mod update;
mod web_serve_handover;
mod web_serve_root;
mod web_serve_shutdown;
mod worktree_resolution;

#[cfg(unix)]
#[test]
fn process_git_fixtures_preserve_inherited_authority_decoy() {
    git_authority::assert_fixtures_preserve_decoy(&[
        "job_resume_detached::unix::auto_refuses_approve_proposed_with_pull",
        "supervised_parent_signal::cli_ctrl_c_during_proc_spawn_reports_interrupt",
        "update::update_preserves_explicit_and_environment_roots_from_another_checkout",
        "web_serve_root::web_serve_without_an_explicit_root_still_serves_the_global_registry",
        "worktree_resolution::config_show_reports_shared_and_local_roots_for_git_worktrees_and_overrides",
    ]);
}
