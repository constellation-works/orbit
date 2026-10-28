//! Verifying a provider branch update that merged the base into the candidate.

use std::path::Path;

use orbit_common::OrbitError;

use super::super::super::git::{git_command_success, git_output};
use super::super::super::pr::PullRequestStatus;

pub(super) fn contains(workspace: &Path, ancestor: &str, descendant: &str) -> bool {
    git_command_success(
        workspace,
        &["merge-base", "--is-ancestor", ancestor, descendant],
    )
    .unwrap_or(false)
}

/// Whether `head` is a provider branch update of the validated `candidate`:
/// a first-parent chain of two-parent merges, each with a base commit the
/// landing ref contains and exactly the clean tree of its parents.
/// Any answer this cannot read is a refusal.
pub(in crate::executor::automation::vcs) fn verified_base_update(
    workspace: &Path,
    candidate: &str,
    head: &str,
    landing_ref: &str,
) -> Result<(), String> {
    let mut current = head.to_string();
    while current != candidate {
        let parents = git_output(workspace, &["rev-list", "--parents", "-n", "1", &current])
            .map_err(|error| format!("head {current} cannot be read: {error}"))?;
        let parents: Vec<&str> = parents.split_whitespace().skip(1).collect();
        let [previous, base_side] = parents.as_slice() else {
            return Err(format!(
                "head {current} is not a two-parent base merge of candidate {candidate}"
            ));
        };
        if !contains(workspace, base_side, landing_ref) {
            return Err(format!(
                "head {current} merges {base_side}, which landing ref '{landing_ref}' does not contain"
            ));
        }
        let merged = git_output(
            workspace,
            &["merge-tree", "--write-tree", previous, base_side],
        )
        .map_err(|error| {
            format!("head {previous} does not merge cleanly with {base_side}: {error}")
        })?;
        let tree = git_output(workspace, &["rev-parse", &format!("{current}^{{tree}}")])
            .map_err(|error| format!("head {current} has no readable tree: {error}"))?;
        if merged.lines().next().map(str::trim) != Some(tree.trim()) {
            return Err(format!(
                "head {current} changes more than the merge of {previous} with {base_side}"
            ));
        }
        current = (*previous).to_string();
    }
    Ok(())
}

/// Shared owner retry/landing proof for a provider base-only update. A cached
/// base never stands in for a successful fetch. Source and native evidence
/// continue to describe the original immutable accepted candidate.
pub(in crate::executor::automation::vcs) fn verify_published_base_update(
    workspace: &Path,
    candidate: &orbit_types::workflow::handoff::HandoffCandidate,
    status: &PullRequestStatus,
) -> Result<(), OrbitError> {
    let refuse =
        |reason: String| OrbitError::Execution(format!("provider base update refused: {reason}"));
    let orbit_types::workflow::handoff::HandoffDelivery::PullRequest { number } =
        candidate.delivery
    else {
        return Err(refuse("published delivery required".into()));
    };
    super::super::super::pr::DeliveryPin::pinned(
        &candidate.source_branch,
        &candidate.base_branch,
        &candidate.candidate.commit,
    )
    .ensure_candidate_identity(status, &number.to_string())?;
    if status.number != Some(number)
        || status.url.as_deref()
            != Some(&format!(
                "https://github.com/{}/pull/{number}",
                candidate.repository
            ))
    {
        return Err(refuse("repository or PR changed".into()));
    }
    let head = status
        .head_sha
        .as_deref()
        .ok_or_else(|| refuse("head unavailable".into()))?;
    if !matches!(head.len(), 40 | 64) || !head.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(refuse(
            "provider head is not an exact commit identity".into(),
        ));
    }
    let remote = git_command_success(workspace, &["remote", "get-url", "origin"])?;
    let landing = if remote {
        super::super::super::git::fetch_remote_base(workspace, &candidate.landing_branch)?;
        format!("origin/{}", candidate.landing_branch)
    } else {
        candidate.landing_branch.clone()
    };
    if !contains(workspace, &candidate.base.commit, &landing) {
        return Err(refuse("landing base rewound or unrelated".into()));
    }
    if remote && !contains(workspace, head, head) {
        let _ = git_command_success(
            workspace,
            &[
                "fetch",
                "--no-tags",
                "--quiet",
                "origin",
                &candidate.source_branch,
            ],
        );
        if !contains(workspace, head, head) {
            // GitHub retains this ref after deleting a merged source branch,
            // including when the landing branch received a squash commit.
            let _ = git_command_success(
                workspace,
                &[
                    "fetch",
                    "--no-tags",
                    "--quiet",
                    "origin",
                    &format!("refs/pull/{number}/head"),
                ],
            );
        }
    }
    verified_base_update(workspace, &candidate.candidate.commit, head, &landing).map_err(refuse)?;
    if head != candidate.candidate.commit {
        let parents = git_output(workspace, &["rev-list", "--parents", "-n", "1", head])?;
        let base_side = parents
            .split_whitespace()
            .nth(2)
            .ok_or_else(|| refuse("base parent unavailable".into()))?;
        if !contains(workspace, &candidate.base.commit, base_side)
            || contains(workspace, base_side, &candidate.candidate.commit)
        {
            return Err(refuse(
                "provider base parent is rewound, unrelated or has not advanced".into(),
            ));
        }
    }
    if matches!(
        super::super::super::pr::classify_pr_state(status),
        super::super::super::pr::PrMergeState::Merged
    ) {
        let merge = status
            .merge_commit
            .as_deref()
            .ok_or_else(|| refuse("merge commit unavailable".into()))?;
        if !matches!(merge.len(), 40 | 64) || !merge.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(refuse(
                "provider merge is not an exact commit identity".into(),
            ));
        }
        if !contains(workspace, merge, &landing) {
            return Err(refuse(
                "reported merge is absent from the fetched landing base".into(),
            ));
        }
        let parents = git_output(workspace, &["rev-list", "--parents", "-n", "1", merge])?;
        let parent = parents
            .split_whitespace()
            .nth(1)
            .ok_or_else(|| refuse("merge parent unavailable".into()))?;
        let clean = git_output(workspace, &["merge-tree", "--write-tree", head, parent])?;
        let tree = git_output(workspace, &["rev-parse", &format!("{merge}^{{tree}}")])?;
        if clean.lines().next().map(str::trim) != Some(tree.trim()) {
            return Err(refuse(
                "reported merge tree does not deliver this provider head".into(),
            ));
        }
    } else if remote {
        let actual = git_output(
            workspace,
            &[
                "ls-remote",
                "--heads",
                "origin",
                &format!("refs/heads/{}", candidate.source_branch),
            ],
        )?;
        if actual.split_whitespace().next() != Some(head) {
            return Err(refuse("remote/provider head lease changed".into()));
        }
    }
    Ok(())
}
