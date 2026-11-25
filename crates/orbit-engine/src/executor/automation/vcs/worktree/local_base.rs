//! Select a fresh immutable starting commit for local admission without moving
//! the owner's branch, index, or working tree. Origin is best effort; a dirty
//! or diverged owner behind origin is refused for operator inspection.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::workflow::DeliveryRefusal;

use super::super::git::{
    fetch_remote_base, git_command_success, git_output, git_output_raw, is_ancestor,
    normalize_base_branch,
};
use super::merge::checkout_holding_branch;

/// Select the fetched commit when it safely fast-forwards the owner base.
pub(super) fn advance_local_base_for_admission(
    process_env: &orbit_common::Env,
    repo_root: &Path,
    base: &str,
) -> Result<Option<String>, OrbitError> {
    let branch = normalize_base_branch(base)?;
    if !git_command_success(process_env, repo_root, &["remote", "get-url", "origin"])? {
        return Ok(None);
    }
    // Keep the selected spelling here: fetch_remote_base normalizes once
    // itself, and passing `branch` would strip a second `origin/` from a
    // literal branch such as `origin/train`.
    if let Err(error) = fetch_remote_base(process_env, repo_root, base) {
        tracing::warn!(
            branch,
            %error,
            "local base freshness skipped: origin fetch failed"
        );
        return Ok(None);
    }
    fast_forward_owner_base(process_env, repo_root, &branch)
}

fn fast_forward_owner_base(
    process_env: &orbit_common::Env,
    repo_root: &Path,
    branch: &str,
) -> Result<Option<String>, OrbitError> {
    let local_ref = format!("refs/heads/{branch}");
    let remote_ref = format!("refs/remotes/origin/{branch}");
    let (Some(owner), Some(origin)) = (
        commit_of(process_env, repo_root, &local_ref)?,
        commit_of(process_env, repo_root, &remote_ref)?,
    ) else {
        // A missing owner branch is reported by the start-point resolution,
        // and an origin without the branch has nothing to advance to.
        return Ok(None);
    };
    // Level with origin, or ahead of it (commits origin does not have yet):
    // the owner's branch is already the newest base this run can use.
    if owner == origin || is_ancestor(process_env, repo_root, &origin, &owner)? {
        return Ok(None);
    }

    let holder = checkout_holding_branch(process_env, repo_root, branch)?;
    if !is_ancestor(process_env, repo_root, &owner, &origin)? {
        return Err(match &holder {
            Some(checkout) => stale_base_refusal(
                checkout,
                branch,
                &owner,
                &origin,
                "has diverged from origin, so it was not moved",
            ),
            None => unattached_stale_base_refusal(branch, &owner, &origin),
        });
    }

    if let Some(checkout) = holder {
        let tracked = git_output_raw(
            process_env,
            &checkout,
            &["status", "--porcelain", "--untracked-files=no"],
        )?;
        if !tracked.is_empty() {
            return Err(stale_base_refusal(
                &checkout,
                branch,
                &owner,
                &origin,
                "has tracked changes, so the run was not admitted",
            ));
        }
    }
    // The selected object is immutable. No operation after holder observation
    // changes the owner's ref or checkout, even if its attachment changes.
    Ok(Some(origin))
}

/// The commit `reference` names, or `None` when the reference does not exist.
fn commit_of(
    process_env: &orbit_common::Env,
    repo_root: &Path,
    reference: &str,
) -> Result<Option<String>, OrbitError> {
    let spec = format!("{reference}^{{commit}}");
    if !git_command_success(
        process_env,
        repo_root,
        &["rev-parse", "--verify", "--quiet", &spec],
    )? {
        return Ok(None);
    }
    git_output(process_env, repo_root, &["rev-parse", "--verify", &spec]).map(Some)
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

/// A diverged base that no checkout holds. Naming a checkout here would point
/// the operator at an unrelated one, so the refusal names the branch itself.
fn unattached_stale_base_refusal(branch: &str, owner: &str, origin: &str) -> OrbitError {
    OrbitError::DeliveryRefused {
        refusal: DeliveryRefusal::StaleBranch,
        message: format!(
            "worktree setup refusing stale base: no checkout holds '{branch}', which is at {owner}, and origin/{branch} is at {origin}; the branch has diverged from origin, so it was not moved. Left it untouched for operator inspection. Reconcile '{branch}' with origin/{branch}, then retry."
        ),
    }
}
