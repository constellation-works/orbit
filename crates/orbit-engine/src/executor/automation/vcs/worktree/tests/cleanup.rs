#![allow(missing_docs)]
use super::super::cleanup::remove_worktree;
use super::support::*;
use std::fs;
use tempfile::tempdir;

/// The removal itself is the last gate: gc calls `remove_worktree` without
/// `--force`, so a worktree dirtied after the status check makes Git refuse.
/// Never replace this with a recursive delete.
#[test]
fn removal_without_force_fails_closed_on_a_dirty_worktree() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let worktree = repo.join(".orbit/state/worktrees/orbit-jrun-noforce");
    add_worktree(&repo, &worktree, "orbit/noforce");
    fs::write(worktree.join("uncommitted.txt"), "valuable").unwrap();

    let error = remove_worktree(&repo, &worktree, Some("orbit/noforce"), false)
        .expect_err("git must refuse to remove a dirty worktree without --force");

    assert!(worktree.join("uncommitted.txt").exists());
    assert!(
        format!("{error}").contains("worktree remove"),
        "unexpected error: {error}"
    );
}

#[cfg(unix)]
#[test]
fn forced_pipeline_cleanup_removes_a_worktree_holding_a_read_only_directory() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let worktree = repo.join(".orbit/state/worktrees/orbit-jrun-forced");
    add_worktree(&repo, &worktree, "orbit/forced");
    let locked = leave_read_only_fixture(&repo, &worktree);
    let _unlock = Unlock(locked);

    remove_worktree(&repo, &worktree, Some("orbit/forced"), true).unwrap();

    assert!(!worktree.exists(), "forced removal must leave nothing");
    assert!(!listed_worktrees(&repo).contains("forced"));
}

#[cfg(unix)]
#[test]
fn repair_never_follows_a_symlink_out_of_the_worktree() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let worktree = repo.join(".orbit/state/worktrees/orbit-jrun-symlinked");
    add_worktree(&repo, &worktree, "orbit/symlinked");
    let outside = temp.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::set_permissions(&outside, fs::Permissions::from_mode(0o555)).unwrap();
    let _unlock = Unlock(outside.clone());
    std::os::unix::fs::symlink(&outside, worktree.join("escape")).unwrap();

    remove_worktree(&repo, &worktree, Some("orbit/symlinked"), true).unwrap();

    assert!(!worktree.exists());
    assert_eq!(
        fs::metadata(&outside).unwrap().permissions().mode() & 0o777,
        0o555,
        "a directory outside the worktree must keep its mode"
    );
}
