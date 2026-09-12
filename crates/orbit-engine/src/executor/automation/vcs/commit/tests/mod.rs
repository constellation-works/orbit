#![allow(missing_docs)]

mod already_landed;
mod author;
mod base_checkpoint;
mod candidate_paths;
mod delivery_gate;
mod git_ops;
mod message;
mod scope;
mod summary;
pub(in crate::executor::automation::vcs::commit) mod test_support;

use std::fs;

use super::super::git::git_success;
use super::worktree_status_counts;

/// ORB-12225: with exactly one dirty entry, `git status --porcelain`'s single
/// line is ` M README.md`. Reading it through `git_output` (rather than
/// `git_output_raw`) trims the leading space, shifting the index/worktree
/// columns by one byte and misclassifying the unstaged entry as staged.
#[test]
fn worktree_status_counts_single_unstaged_modification() {
    let temp = test_support::initialized_git_repo();
    let repo = temp.path();
    fs::write(repo.join("README.md"), "changed\n").expect("modify tracked file");

    let counts = worktree_status_counts(repo).expect("read worktree status");

    assert_eq!(counts.staged, 0);
    assert_eq!(counts.unstaged, 1);
    assert_eq!(counts.untracked, 0);
}

#[test]
fn worktree_status_counts_multi_entry_mix() {
    let temp = test_support::initialized_git_repo();
    let repo = temp.path();
    fs::write(repo.join("README.md"), "changed\n").expect("modify tracked file");
    fs::write(repo.join("staged.txt"), "staged\n").expect("write staged file");
    git_success(repo, &["add", "staged.txt"]).expect("stage new file");
    fs::write(repo.join("untracked.txt"), "untracked\n").expect("write untracked file");

    let counts = worktree_status_counts(repo).expect("read worktree status");

    assert_eq!(counts.staged, 1);
    assert_eq!(counts.unstaged, 1);
    assert_eq!(counts.untracked, 1);
}
