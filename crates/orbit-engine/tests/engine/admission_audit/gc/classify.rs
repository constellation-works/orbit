#![allow(missing_docs)]
use super::support::*;
use orbit_engine::{WorktreeGcOptions, collect_worktrees};
use orbit_types::task::TaskStatus;
use orbit_types::workflow::JobRunState;
use std::fs;
use tempfile::tempdir;

/// A directory whose `.git` link still resolves may be a moved worktree that
/// `git worktree repair` brings back, so GC leaves it for an operator.
fn gc_keeps_an_unregistered_directory_whose_git_link_still_resolves() {
    if !super::super::run_isolated(concat!(
        module_path!(),
        "::gc_keeps_an_unregistered_directory_whose_git_link_still_resolves"
    )) {
        return;
    }

    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let run = pipeline_run("jrun-moved", JobRunState::Success, &["ORB-MOVED"]);
    let worktree = resolved_task_worktree(&repo, &run);
    fs::create_dir_all(&worktree).unwrap();
    fs::write(
        worktree.join(".git"),
        format!("gitdir: {}\n", repo.join(".git").display()),
    )
    .unwrap();
    fs::write(worktree.join("work.txt"), "precious").unwrap();
    let host = FakeTaskHost::new(vec![task_fixture("ORB-MOVED", TaskStatus::Done)]);

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

    assert_eq!(result.reports[0].action, "skipped:not_registered_worktree");
    assert!(worktree.join("work.txt").exists());
}

/// A leftover is reclaimed only on the same gates as any worktree.
fn gc_keeps_a_leftover_whose_task_is_not_settled() {
    if !super::super::run_isolated(concat!(
        module_path!(),
        "::gc_keeps_a_leftover_whose_task_is_not_settled"
    )) {
        return;
    }

    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let run = pipeline_run("jrun-open", JobRunState::Success, &["ORB-OPEN"]);
    let worktree = resolved_task_worktree(&repo, &run);
    fs::create_dir_all(&worktree).unwrap();
    fs::write(worktree.join("work.txt"), "unsettled").unwrap();
    let host = FakeTaskHost::new(vec![task_fixture("ORB-OPEN", TaskStatus::Review)]);

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

    assert_eq!(result.reports[0].action, "skipped:task_status_ineligible");
    assert!(worktree.join("work.txt").exists());
}

#[test]
fn gc_ownership_contracts() {
    super::super::run_family(
        concat!(module_path!(), "::gc_ownership_contracts"),
        module_path!(),
        &[
            (
                "gc_keeps_an_unregistered_directory_whose_git_link_still_resolves",
                gc_keeps_an_unregistered_directory_whose_git_link_still_resolves,
            ),
            (
                "gc_keeps_a_leftover_whose_task_is_not_settled",
                gc_keeps_a_leftover_whose_task_is_not_settled,
            ),
        ],
    );
}
