//! Owner-base freshness for `base_sync: local` admission [ORB-28152].
//!
//! A local run cuts its worktree from `refs/heads/<base>` of the owner
//! checkout. A commit pushed to `origin/<base>` from another checkout never
//! reaches that branch by itself, so the run would build on an old base. Before
//! the worktree is cut, a clean owner whose branch is a strict ancestor of the
//! fetched origin tip is fast-forwarded to it. A dirty or diverged owner is
//! never moved: setup refuses with the stale-branch hold, which names the
//! checkout and both commits and leaves the branch and checkout for inspection.
//!
//! Origin is best effort here, because local mode is designed to run without
//! one. A repository with no `origin` remote, or an origin that cannot be
//! fetched, keeps cutting from the owner's branch as before.
//!
//! A fast-forward never overwrites a file the owner's status cannot see: an
//! ignored file at a path origin starts tracking refuses the advance, naming
//! the path, and the branch, index and file stay as they were.
//!
//! The fetch takes the shared fetch lock. The fast-forward itself does not
//! take it: the owner's ref moves through Git's own compare-and-swap (or a
//! checked-out `merge --ff-only`), so a concurrent movement makes this step
//! fail rather than corrupt the branch.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::workflow::DeliveryRefusal;

use super::super::git::{
    fetch_remote_base, git_command_success, git_output, git_output_raw, is_ancestor,
    normalize_base_branch,
};
use super::merge::checkout_holding_branch;

/// Advance the owner's `<base>` to a fast-forward of `origin/<base>` before a
/// local run is admitted, or refuse without moving it.
pub(super) fn advance_local_base_for_admission(
    repo_root: &Path,
    base: &str,
) -> Result<(), OrbitError> {
    let branch = normalize_base_branch(base)?;
    if !git_command_success(repo_root, &["remote", "get-url", "origin"])? {
        return Ok(());
    }
    if let Err(error) = fetch_remote_base(repo_root, &branch) {
        tracing::warn!(
            branch,
            %error,
            "local base freshness skipped: origin fetch failed"
        );
        return Ok(());
    }
    fast_forward_owner_base(repo_root, &branch)
}

fn fast_forward_owner_base(repo_root: &Path, branch: &str) -> Result<(), OrbitError> {
    let local_ref = format!("refs/heads/{branch}");
    let remote_ref = format!("refs/remotes/origin/{branch}");
    let (Some(owner), Some(origin)) = (
        commit_of(repo_root, &local_ref)?,
        commit_of(repo_root, &remote_ref)?,
    ) else {
        // A missing owner branch is reported by the start-point resolution,
        // and an origin without the branch has nothing to advance to.
        return Ok(());
    };
    // Level with origin, or ahead of it (commits origin does not have yet):
    // the owner's branch is already the newest base this run can use.
    if owner == origin || is_ancestor(repo_root, &origin, &owner)? {
        return Ok(());
    }

    let holder = checkout_holding_branch(repo_root, branch)?;
    let location = holder.clone().unwrap_or_else(|| repo_root.to_path_buf());
    if !is_ancestor(repo_root, &owner, &origin)? {
        return Err(stale_base_refusal(
            &location,
            branch,
            &owner,
            &origin,
            "has diverged from origin, so it was not moved",
        ));
    }

    match holder {
        // No checkout holds the branch, so no tree can be disturbed: move the
        // ref itself, and only from the commit this step just read.
        None => {
            if git_command_success(repo_root, &["update-ref", &local_ref, &origin, &owner])? {
                return Ok(());
            }
            Err(stale_base_refusal(
                &location,
                branch,
                &owner,
                &origin,
                "moved while it was being advanced",
            ))
        }
        Some(checkout) => {
            let tracked = git_output_raw(
                &checkout,
                &["status", "--porcelain", "--untracked-files=no"],
            )?;
            if !tracked.is_empty() {
                return Err(stale_base_refusal(
                    &checkout,
                    branch,
                    &owner,
                    &origin,
                    "has tracked changes, so it was not fast-forwarded",
                ));
            }
            // An ignored owner file is invisible to the status above, so a
            // path origin starts tracking would be overwritten silently. Name
            // it up front; the merge flag below is the backstop (STD-03 §R29).
            let collisions = ignored_collisions(&checkout, &owner, &origin)?;
            if !collisions.is_empty() {
                return Err(stale_base_refusal(
                    &checkout,
                    branch,
                    &owner,
                    &origin,
                    &format!(
                        "has ignored files that origin would overwrite ({}), so it was not fast-forwarded",
                        collisions.join(", ")
                    ),
                ));
            }
            if git_command_success(
                &checkout,
                &["merge", "--ff-only", "--no-overwrite-ignore", &origin],
            )? {
                return Ok(());
            }
            Err(stale_base_refusal(
                &checkout,
                branch,
                &owner,
                &origin,
                "could not be fast-forwarded by Git, so it was not moved",
            ))
        }
    }
}

/// Ignored files in `checkout` at paths the advance from `owner` to `origin`
/// adds, which a fast-forward would replace with origin's bytes.
fn ignored_collisions(
    checkout: &Path,
    owner: &str,
    origin: &str,
) -> Result<Vec<String>, OrbitError> {
    let added = git_output_raw(
        checkout,
        &[
            "diff",
            "--name-only",
            "--no-renames",
            "--diff-filter=A",
            "-z",
            owner,
            origin,
        ],
    )?;
    let present: Vec<String> = added
        .split('\0')
        .filter(|path| !path.is_empty())
        .filter(|path| checkout.join(path).symlink_metadata().is_ok())
        .map(|path| format!(":(literal){path}"))
        .collect();
    let mut collisions = Vec::new();
    for chunk in present.chunks(256) {
        let mut args = vec![
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "-z",
            "--",
        ];
        args.extend(chunk.iter().map(String::as_str));
        let listed = git_output_raw(checkout, &args)?;
        collisions.extend(
            listed
                .split('\0')
                .filter(|path| !path.is_empty())
                .map(str::to_string),
        );
    }
    Ok(collisions)
}

/// The commit `reference` names, or `None` when the reference does not exist.
fn commit_of(repo_root: &Path, reference: &str) -> Result<Option<String>, OrbitError> {
    let spec = format!("{reference}^{{commit}}");
    if !git_command_success(repo_root, &["rev-parse", "--verify", "--quiet", &spec])? {
        return Ok(None);
    }
    git_output(repo_root, &["rev-parse", "--verify", &spec]).map(Some)
}

fn stale_base_refusal(
    checkout: &Path,
    branch: &str,
    owner: &str,
    origin: &str,
    reason: &str,
) -> OrbitError {
    OrbitError::DeliveryRefused {
        refusal: DeliveryRefusal::StaleBranch,
        message: format!(
            "worktree setup refusing stale base: owner checkout '{}' holds '{branch}' at {owner}, and origin/{branch} is at {origin}; the checkout {reason}. Left it untouched for operator inspection. Reconcile the checkout with origin, then retry.",
            checkout.display()
        ),
    }
}
