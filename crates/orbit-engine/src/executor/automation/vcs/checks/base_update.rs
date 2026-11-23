//! Verifying a provider branch update that merged the base into the candidate.

use std::path::Path;

use orbit_common::OrbitError;

use super::super::git::{git_command_success, git_output};
use super::super::pr_answers::PullRequestStatus;

pub(super) fn contains(workspace: &Path, ancestor: &str, descendant: &str) -> bool {
    crate::executor::automation::vcs::git::is_ancestor(workspace, ancestor, descendant)
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

/// Verify that `head` is either `reviewed_head` itself or a first-parent chain
/// of conflict-free two-parent merge commits of `base_branch` onto
/// `reviewed_head`, with no conflict resolution or other edits.
///
/// Any commit after `reviewed_head` that is not a clean two-parent merge of a
/// commit contained in `base_branch` (including a regular edit commit, a
/// conflict resolution, or merging a non-base branch) is refused.
pub fn verify_refreshed_head_from_base(
    workspace: &Path,
    reviewed_head: &str,
    head: &str,
    base_branch: &str,
    head_branch: Option<&str>,
    pr_number: Option<u64>,
) -> Result<(), String> {
    if head == reviewed_head {
        return Ok(());
    }
    let valid_sha = |sha: &str| {
        matches!(sha.len(), 40 | 64) && sha.bytes().all(|byte| byte.is_ascii_hexdigit())
    };
    if !valid_sha(reviewed_head) || !valid_sha(head) {
        return Err("refreshed head verification requires exact commit SHAs".to_string());
    }
    let has_remote =
        git_command_success(workspace, &["remote", "get-url", "origin"]).unwrap_or(false);
    let landing_ref = if has_remote {
        super::super::git::fetch_remote_base(workspace, base_branch)
            .map_err(|error| format!("could not fetch base branch '{base_branch}': {error}"))?;
        format!("origin/{base_branch}")
    } else {
        base_branch.to_string()
    };
    if has_remote && !contains(workspace, head, head) {
        if let Some(branch) = head_branch.filter(|b| !b.trim().is_empty()) {
            let _ = git_command_success(
                workspace,
                &["fetch", "--no-tags", "--quiet", "origin", branch],
            );
        }
        if !contains(workspace, head, head)
            && let Some(number) = pr_number
        {
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
        if !contains(workspace, head, head) {
            let _ = git_command_success(
                workspace,
                &["fetch", "--no-tags", "--quiet", "origin", head],
            );
        }
    }
    if has_remote && !contains(workspace, reviewed_head, reviewed_head) {
        let _ = git_command_success(
            workspace,
            &["fetch", "--no-tags", "--quiet", "origin", reviewed_head],
        );
    }
    if git_output(
        workspace,
        &[
            "rev-parse",
            "--verify",
            &format!("{landing_ref}^{{commit}}"),
        ],
    )
    .is_err()
    {
        return Err(format!("base ref '{landing_ref}' cannot be resolved"));
    }
    if !contains(workspace, reviewed_head, head) {
        return Err(format!(
            "head {head} does not descend from reviewed head {reviewed_head}"
        ));
    }
    verified_base_update(workspace, reviewed_head, head, &landing_ref)
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
    super::super::pr_answers::DeliveryPin::pinned(
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
    let landing_ref = if remote {
        super::super::git::fetch_remote_base(workspace, &candidate.landing_branch)?;
        format!("origin/{}", candidate.landing_branch)
    } else {
        candidate.landing_branch.clone()
    };
    if !contains(workspace, &candidate.base.commit, &landing_ref) {
        return Err(refuse("landing base rewound or unrelated".into()));
    }
    if remote {
        fetch_provider_head(workspace, head, &candidate.source_branch, number);
    }
    verified_base_update(workspace, &candidate.candidate.commit, head, &landing_ref)
        .map_err(refuse)?;
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
        super::super::pr_answers::classify_pr_state(status),
        super::super::pr_answers::PrMergeState::Merged
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
        merge_delivers_head(workspace, merge, head, &landing_ref).map_err(refuse)?;
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

/// Fetch a provider head the checkout cannot read yet: first its source
/// branch, then the pull request's retained ref. Fetch failures are not
/// answers; the caller's later reads refuse an object still missing.
fn fetch_provider_head(workspace: &Path, head: &str, source_branch: &str, number: u64) {
    if contains(workspace, head, head) {
        return;
    }
    let _ = git_command_success(
        workspace,
        &["fetch", "--no-tags", "--quiet", "origin", source_branch],
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

/// Whether the reported `merge` is on `landing_ref` and its tree is exactly
/// the clean merge of `head` into the merge's first parent, the landing
/// branch tip it merged onto. Returns that first parent.
fn merge_delivers_head(
    workspace: &Path,
    merge: &str,
    head: &str,
    landing_ref: &str,
) -> Result<String, String> {
    if !contains(workspace, merge, landing_ref) {
        return Err("reported merge is absent from the fetched landing base".into());
    }
    let parents = git_output(workspace, &["rev-list", "--parents", "-n", "1", merge])
        .map_err(|error| format!("merge {merge} cannot be read: {error}"))?;
    let parent = parents
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| "merge parent unavailable".to_string())?
        .to_string();
    let clean = git_output(workspace, &["merge-tree", "--write-tree", head, &parent])
        .map_err(|error| format!("head {head} does not merge cleanly onto {parent}: {error}"))?;
    let tree = git_output(workspace, &["rev-parse", &format!("{merge}^{{tree}}")])
        .map_err(|error| format!("merge {merge} has no readable tree: {error}"))?;
    if clean.lines().next().map(str::trim) != Some(tree.trim()) {
        return Err("reported merge tree does not deliver this provider head".into());
    }
    Ok(parent)
}

/// A pull request the forge reports merged, and the head its stopped
/// delivery published.
#[derive(Debug, Clone, Copy)]
pub struct MergedBaseRefresh<'a> {
    /// The exact head the delivery published and validated.
    pub published_head: &'a str,
    /// The head the pull request merged at.
    pub merged_head: &'a str,
    /// The pull request's source branch.
    pub source_branch: &'a str,
    /// The branch it merged into.
    pub landing_branch: &'a str,
    /// The pull request's number.
    pub pull_request: u64,
    /// The commit it merged as.
    pub merge_commit: &'a str,
}

/// Verify that a pull request merged at a head other than its published one
/// delivered only the published candidate plus target-branch commits: the
/// merged head reaches the published head through clean two-parent base
/// merges (the provider branch-update rule), each base side already on the landing
/// branch before this merge, and the merge commit's tree is exactly that
/// head merged onto the landing tip. Base sides are checked against the
/// merge's first parent, not the landing branch after it, so a commit that
/// reached the landing branch only through this merge never counts as base.
/// Any commit outside the candidate plus base, an unreadable object or an
/// unclean merge refuses.
pub fn verify_merged_base_refresh(
    workspace: &Path,
    refresh: &MergedBaseRefresh<'_>,
) -> Result<(), OrbitError> {
    let refuse = |reason: String| {
        OrbitError::Execution(format!(
            "merged head {} is not the published head {} plus base merges: {reason}",
            refresh.merged_head, refresh.published_head
        ))
    };
    for commit in [
        refresh.published_head,
        refresh.merged_head,
        refresh.merge_commit,
    ] {
        if !matches!(commit.len(), 40 | 64) || !commit.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(refuse(format!("{commit} is not an exact commit identity")));
        }
    }
    let remote = git_command_success(workspace, &["remote", "get-url", "origin"])?;
    let landing_ref = if remote {
        super::super::git::fetch_remote_base(workspace, refresh.landing_branch)?;
        fetch_provider_head(
            workspace,
            refresh.merged_head,
            refresh.source_branch,
            refresh.pull_request,
        );
        format!("origin/{}", refresh.landing_branch)
    } else {
        refresh.landing_branch.to_string()
    };
    let onto = merge_delivers_head(
        workspace,
        refresh.merge_commit,
        refresh.merged_head,
        &landing_ref,
    )
    .map_err(refuse)?;
    verified_base_update(
        workspace,
        refresh.published_head,
        refresh.merged_head,
        &onto,
    )
    .map_err(refuse)
}
