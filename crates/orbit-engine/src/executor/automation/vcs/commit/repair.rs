//! Staging and committing a reviewer's repairs under the reviewer's identity.

use std::path::Path;

use orbit_common::OrbitError;

use super::super::git::git_success;
use super::author::{orbit_committers_accepted, reviewer_author, reviewer_authors_accepted};
use super::git_ops::{
    ensure_named_branch, ensure_no_unmerged_changes, git_commit_as, staged_changed_files,
};

/// Stage every worktree change for a reviewer repair commit [ORB-11333].
pub(in crate::executor::automation::vcs) fn stage_everything(
    workspace_path: &Path,
) -> Result<(), OrbitError> {
    ensure_named_branch(workspace_path)?;
    ensure_no_unmerged_changes(workspace_path)?;
    git_success(workspace_path, &["add", "--all", "--", "."])
}

/// The staged paths, relative to the worktree.
pub(in crate::executor::automation::vcs) fn staged_paths(
    workspace_path: &Path,
) -> Result<Vec<String>, OrbitError> {
    staged_changed_files(workspace_path)
}

/// Commit the staged reviewer repairs under the reviewer's own identity.
pub(in crate::executor::automation::vcs) fn commit_reviewer_repairs_in(
    workspace_path: &Path,
    reviewer_model: &str,
    message: &str,
) -> Result<(), OrbitError> {
    git_commit_as(workspace_path, message, &reviewer_author(reviewer_model))
}

/// The `name <email>` author and committer pairs Git may record on a
/// reviewer repair commit: the current identities, and for the
/// compatibility window the ones written before the single scheme.
pub(in crate::executor::automation::vcs) fn reviewer_repair_identities(
    reviewer_model: &str,
) -> Vec<(String, String)> {
    let committers = orbit_committers_accepted();
    reviewer_authors_accepted(reviewer_model)
        .iter()
        .flat_map(|author| {
            committers
                .iter()
                .map(|committer| (author.spec(), committer.spec()))
        })
        .collect()
}
