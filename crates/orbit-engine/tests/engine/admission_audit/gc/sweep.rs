#![allow(missing_docs)]
use super::support::*;
use orbit_engine::{WorktreeGcOptions, collect_worktrees};
use orbit_types::task::TaskStatus;
use orbit_types::workflow::JobRunState;
use std::fs;
use tempfile::tempdir;

/// Safety gate: the collector never follows a symlink standing where a
/// worktree should be.
#[cfg(unix)]
fn symlink_at_a_known_worktree_path_is_never_followed() {
    if !super::super::run_isolated(concat!(
        module_path!(),
        "::symlink_at_a_known_worktree_path_is_never_followed"
    )) {
        return;
    }

    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let run = pipeline_run("jrun-symlink", JobRunState::Success, &["ORB-SYMLINK"]);
    let worktree = resolved_task_worktree(&repo, &run);
    let elsewhere = temp.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::write(elsewhere.join("precious.txt"), "keep me").unwrap();
    fs::create_dir_all(worktree.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &worktree).unwrap();
    let host = FakeTaskHost::new(vec![task_fixture("ORB-SYMLINK", TaskStatus::Done)]);

    let result = collect_worktrees(
        &repo,
        &[run],
        &host,
        &WorktreeGcOptions {
            delete: true,
            ..Default::default()
        },
    )
    .unwrap();

    assert!(elsewhere.join("precious.txt").exists());
    assert_eq!(result.reports[0].action, "skipped:not_a_real_directory");
}

#[cfg(unix)]
fn gc_removes_a_worktree_holding_a_read_only_directory() {
    if !super::super::run_isolated(concat!(
        module_path!(),
        "::gc_removes_a_worktree_holding_a_read_only_directory"
    )) {
        return;
    }

    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let run = pipeline_run("jrun-readonly", JobRunState::Success, &["ORB-READONLY"]);
    let worktree = resolved_task_worktree(&repo, &run);
    add_worktree(&repo, &worktree, "orbit/readonly");
    let locked = leave_read_only_fixture(&repo, &worktree);
    let _unlock = Unlock(locked);
    let host = FakeTaskHost::new(vec![task_fixture("ORB-READONLY", TaskStatus::Done)]);

    let result = collect_worktrees(
        &repo,
        &[run],
        &host,
        &WorktreeGcOptions {
            delete: true,
            ..Default::default()
        },
    )
    .unwrap();

    assert_eq!(result.reports[0].action, "removed");
    assert!(result.reports[0].bytes_reclaimed > 0);
    assert!(!worktree.exists(), "the whole directory must be gone");
    assert!(!listed_worktrees(&repo).contains("readonly"));
}

#[test]
fn gc_symlink_and_read_only_contracts() {
    super::super::run_family(
        concat!(module_path!(), "::gc_symlink_and_read_only_contracts"),
        module_path!(),
        &[
            #[cfg(unix)]
            (
                "symlink_at_a_known_worktree_path_is_never_followed",
                symlink_at_a_known_worktree_path_is_never_followed,
            ),
            #[cfg(unix)]
            (
                "gc_removes_a_worktree_holding_a_read_only_directory",
                gc_removes_a_worktree_holding_a_read_only_directory,
            ),
        ],
    );
}
