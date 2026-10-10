//! [ORB-15308] A claimed leaf that resumes a published candidate with a pull
//! request continues on that candidate's branch instead of its own.
//!
//! The leaf's worktree starts on a branch named for its own run. Left there,
//! the resumed work is pushed to a new branch and `pr_open` opens a second
//! pull request beside the one the earlier claim opened. Renaming this
//! checkout's branch to the candidate's lets `git_push` replace the published
//! head under a lease on it (`reused_head_sha`) and `pr_open` find the
//! same pull request by its head. The candidate's changes are already applied
//! as uncommitted edits, so only the branch name moves.
//!
//! On the host that made the candidate, its retained worktree may still hold
//! that branch. The branch is then renamed aside to
//! `<branch>-superseded-<suffix>` first, which keeps that checkout and its
//! commits intact, and worktree GC still reads the branch from its HEAD.
//! A local branch at any commit other than the published head is never
//! moved: the leaf keeps its own branch, and `pr_open` closes the earlier
//! pull request once its own is open.

use std::path::Path;

use orbit_common::OrbitError;

use super::super::git::{git_command_success, git_output, git_run};
use super::Candidate;

/// Whether this checkout continues on the candidate's branch.
pub(super) enum Adoption {
    /// This checkout's branch is now the candidate's.
    Adopted,
    /// Why the leaf keeps its own branch.
    Refused(String),
}

/// Rename this checkout's branch to `candidate`'s published branch, moving a
/// retained worktree's copy of it aside when it sits at the published head.
pub(super) fn adopt_published_branch(
    workspace_path: &Path,
    task_id: &str,
    candidate: &Candidate,
) -> Result<Adoption, OrbitError> {
    let branch = candidate.branch.as_str();
    let Ok(ours) = git_output(
        workspace_path,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
    ) else {
        return Ok(Adoption::Refused(
            "this checkout has a detached HEAD".to_string(),
        ));
    };
    let ours = ours.trim().to_string();
    if ours == branch {
        return Ok(Adoption::Adopted);
    }
    // Only another delivery branch of this task, in this checkout's branch
    // namespace, is adopted: the kept reference names nothing else.
    let ours_prefix = ours.rsplit_once('/').map(|(prefix, _)| prefix);
    let task_branch = branch.rsplit_once('/').is_some_and(|(prefix, name)| {
        Some(prefix) == ours_prefix && name.starts_with(&format!("{task_id}-"))
    });
    if !task_branch
        || !git_command_success(workspace_path, &["check-ref-format", "--branch", branch])?
    {
        return Ok(Adoption::Refused(format!(
            "'{branch}' is not one of this task's delivery branches beside '{ours}'"
        )));
    }

    let local_ref = format!("refs/heads/{branch}");
    if git_command_success(
        workspace_path,
        &["show-ref", "--verify", "--quiet", &local_ref],
    )? {
        let tip = git_output(
            workspace_path,
            &["rev-parse", "--verify", &format!("{local_ref}^{{commit}}")],
        )?;
        if tip.trim() != candidate.head_sha {
            return Ok(Adoption::Refused(format!(
                "this host's branch '{branch}' is at {}, not the published head {}",
                tip.trim(),
                candidate.head_sha
            )));
        }
        if held_by_worktree(workspace_path, &local_ref)? {
            let suffix = ours.rsplit('-').next().unwrap_or(ours.as_str());
            let aside = format!("{branch}-superseded-{suffix}");
            if let Some(refused) = rename(workspace_path, &["branch", "-m", branch, &aside])? {
                return Ok(Adoption::Refused(refused));
            }
        }
    }
    // A local copy no worktree holds sits at the published head, which
    // `origin` keeps, so `-M` loses nothing by replacing it.
    Ok(
        match rename(workspace_path, &["branch", "-M", &ours, branch])? {
            Some(refused) => Adoption::Refused(refused),
            None => Adoption::Adopted,
        },
    )
}

/// Whether a registered worktree has `local_ref` checked out.
fn held_by_worktree(workspace_path: &Path, local_ref: &str) -> Result<bool, OrbitError> {
    let listed = git_output(workspace_path, &["worktree", "list", "--porcelain"])?;
    Ok(listed
        .lines()
        .any(|line| line.strip_prefix("branch ") == Some(local_ref)))
}

/// Run a branch rename; `Some` is why Git refused it.
fn rename(workspace_path: &Path, args: &[&str]) -> Result<Option<String>, OrbitError> {
    let outcome = git_run(workspace_path, args)?;
    if outcome.success {
        return Ok(None);
    }
    Ok(Some(format!(
        "`git {}` failed: {}",
        args.join(" "),
        outcome.stderr.trim()
    )))
}
