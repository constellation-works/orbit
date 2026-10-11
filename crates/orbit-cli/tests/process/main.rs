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
#[cfg(unix)]
#[path = "../support/workflow_gate.rs"]
mod workflow_gate;

mod env_pass_warning;
mod job_resume_detached;
mod run_observation;
mod supervised_parent_signal;
mod update;
#[cfg(unix)]
mod web_fixture_io;
mod web_serve_handover;
mod web_serve_root;
mod web_serve_shutdown;
mod worktree_resolution;

#[cfg(unix)]
#[test]
fn process_git_fixtures_preserve_inherited_authority_decoy() {
    const CTRL_C: &str = "supervised_parent_signal::cli_ctrl_c_during_proc_spawn_reports_interrupt";
    let mut fixtures = vec![
        "job_resume_detached::unix::auto_refuses_approve_proposed_with_pull",
        "update::update_preserves_explicit_and_environment_roots_from_another_checkout",
        "web_serve_root::web_serve_without_an_explicit_root_still_serves_the_global_registry",
        "worktree_resolution::config_show_reports_shared_and_local_roots_for_git_worktrees_and_overrides",
    ];
    // The Ctrl-C fixture needs Seatbelt on macOS, which a nested agent sandbox
    // refuses to apply. Its notice is printed here, where the run's output
    // shows it; a child's would stay in the captured child output [ORB-15409].
    if orbit_exec::macos_sandbox_test_guard(&format!(
        "process_git_fixtures_preserve_inherited_authority_decoy: {CTRL_C}"
    )) {
        fixtures.insert(1, CTRL_C);
    }
    git_authority::assert_fixtures_preserve_decoy(&fixtures);
}
