use std::fs;
use std::path::Path;
use std::process::Command;

use orbit_common::test_env;
use tempfile::tempdir;

use crate::paths::{find_git_main_worktree_root, may_be_linked_worktree};

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.email=orbit@example.com",
            "-c",
            "user.name=orbit",
        ])
        .args(args)
        .status()
        .expect("git runs");
    assert!(status.success(), "git {args:?} failed");
}

#[test]
fn ordinary_checkouts_and_plain_directories_cannot_be_linked_worktrees() {
    let _env = test_env::unset(["GIT_DIR", "GIT_COMMON_DIR", "GIT_WORK_TREE"]);
    let plain = tempdir().expect("plain");
    let repo = tempdir().expect("repo");
    fs::create_dir_all(repo.path().join(".git")).expect("git dir");
    let nested = repo.path().join("a").join("b");
    fs::create_dir_all(&nested).expect("nested");

    assert!(!may_be_linked_worktree(plain.path()));
    assert!(!may_be_linked_worktree(repo.path()));
    assert!(!may_be_linked_worktree(&nested));
}

#[test]
fn gitfiles_and_common_dir_pointers_may_be_linked_worktrees() {
    let _env = test_env::unset(["GIT_DIR", "GIT_COMMON_DIR", "GIT_WORK_TREE"]);
    let linked = tempdir().expect("linked");
    fs::write(
        linked.path().join(".git"),
        "gitdir: /elsewhere/.git/worktrees/x\n",
    )
    .expect("gitfile");
    let nested = linked.path().join("src");
    fs::create_dir_all(&nested).expect("nested");
    let pointed = tempdir().expect("pointed");
    fs::create_dir_all(pointed.path().join(".git")).expect("git dir");
    fs::write(pointed.path().join(".git").join("commondir"), "../..\n").expect("commondir");

    assert!(may_be_linked_worktree(linked.path()));
    assert!(may_be_linked_worktree(&nested));
    assert!(may_be_linked_worktree(pointed.path()));
}

#[test]
fn nearest_git_entry_decides_even_inside_an_outer_repository() {
    let _env = test_env::unset(["GIT_DIR", "GIT_COMMON_DIR", "GIT_WORK_TREE"]);
    let outer = tempdir().expect("outer");
    fs::write(
        outer.path().join(".git"),
        "gitdir: /elsewhere/.git/worktrees/x\n",
    )
    .expect("outer gitfile");
    let inner = outer.path().join("vendor").join("dep");
    fs::create_dir_all(inner.join(".git")).expect("inner git dir");

    assert!(!may_be_linked_worktree(&inner));
}

#[test]
fn git_location_overrides_defer_to_git() {
    let repo = tempdir().expect("repo");
    fs::create_dir_all(repo.path().join(".git")).expect("git dir");
    for name in ["GIT_DIR", "GIT_COMMON_DIR", "GIT_WORK_TREE"] {
        let _env = test_env::scoped([(name, Some("/somewhere"))]);
        assert!(may_be_linked_worktree(repo.path()), "{name}");
    }
}

#[test]
fn main_worktree_root_is_found_only_from_a_real_linked_worktree() {
    let _env = test_env::unset(["GIT_DIR", "GIT_COMMON_DIR", "GIT_WORK_TREE"]);
    let parent = tempdir().expect("parent");
    // Git reports resolved paths; start from one so the comparison is exact.
    let parent = parent.path().canonicalize().expect("canonical parent");
    let main = parent.join("main");
    let linked = parent.join("linked");
    fs::create_dir_all(&main).expect("main");
    git(&main, &["init", "-q"]);
    git(&main, &["commit", "-q", "--allow-empty", "-m", "init"]);
    git(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            linked.to_str().expect("utf8"),
            "-b",
            "wt",
        ],
    );
    let nested = main.join("src");
    fs::create_dir_all(&nested).expect("nested");

    assert_eq!(find_git_main_worktree_root(&main), None);
    assert_eq!(find_git_main_worktree_root(&nested), None);
    assert_eq!(find_git_main_worktree_root(&linked), Some(main.clone()));
}
