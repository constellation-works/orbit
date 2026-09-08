//! Git mechanics for the before-PR review gate [ORB-11333].
//!
//! The engine collects candidate identity (base and head commits and trees,
//! the implementation commits with the attribution Git recorded) and appends
//! reviewer-authored repair commits with the reviewer's own agent-family
//! identity. Whether a gate applies, who may review, what the verdict means
//! and where the evidence is persisted are Core decisions.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::workflow::CommitIdentity;
use orbit_types::workflow::automation::SourceRevision;

use super::commit::{commit_reviewer_repairs_in, stage_everything, staged_paths};
use super::git::{
    base_sync_mode_from_input, git_output, git_output_raw, git_success,
    resolve_worktree_start_point,
};

/// The pinned candidate a reviewer is handed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateIdentity {
    pub base: SourceRevision,
    pub head: SourceRevision,
    /// Commits in `base..head`, oldest first.
    pub commits: Vec<CommitIdentity>,
}

/// Resolve a commit-ish to its commit and tree ids.
pub fn revision(workspace_path: &Path, spec: &str) -> Result<SourceRevision, OrbitError> {
    let commit = git_output(
        workspace_path,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{spec}^{{commit}}"),
        ],
    )?;
    let tree = git_output(
        workspace_path,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{commit}^{{tree}}"),
        ],
    )?;
    Ok(SourceRevision { commit, tree })
}

/// The base the checked-out candidate was synchronized onto: the merge base
/// of HEAD and the configured base ref under the run's sync mode. After the
/// pipeline's rebase step this is the base tip the candidate sits on.
pub fn synchronized_base(
    workspace_path: &Path,
    base: &str,
    base_sync: &str,
) -> Result<SourceRevision, OrbitError> {
    let mode = base_sync_mode_from_input(&serde_json::json!({ "base_sync": base_sync }))?;
    let base_ref = resolve_worktree_start_point(workspace_path, base, mode)?;
    let merge_base = git_output(workspace_path, &["merge-base", "HEAD", &base_ref])?;
    revision(workspace_path, &merge_base)
}

/// The candidate currently checked out, pinned against `base_sha`.
pub fn candidate_identity(
    workspace_path: &Path,
    base_sha: &str,
) -> Result<CandidateIdentity, OrbitError> {
    let base = revision(workspace_path, base_sha)?;
    let head = revision(workspace_path, "HEAD")?;
    let commits = commits_between(workspace_path, &base.commit, &head.commit)?;
    Ok(CandidateIdentity {
        base,
        head,
        commits,
    })
}

/// Commits in `base..head` with the attribution Git recorded, oldest first.
pub fn commits_between(
    workspace_path: &Path,
    base: &str,
    head: &str,
) -> Result<Vec<CommitIdentity>, OrbitError> {
    let raw = git_output_raw(
        workspace_path,
        &[
            "log",
            "--reverse",
            "--first-parent",
            "--format=%H%x1f%T%x1f%an <%ae>%x1f%cn <%ce>%x1f%s%x1e",
            &format!("{base}..{head}"),
        ],
    )?;
    raw.split('\u{1e}')
        .map(str::trim)
        .filter(|record| !record.is_empty())
        .map(|record| {
            let mut fields = record.split('\u{1f}');
            let mut next = |name: &str| {
                fields.next().map(str::to_string).ok_or_else(|| {
                    OrbitError::Execution(format!(
                        "git log omitted the {name} field for a candidate commit"
                    ))
                })
            };
            Ok(CommitIdentity {
                commit: next("commit")?,
                tree: next("tree")?,
                author: next("author")?,
                committer: next("committer")?,
                subject: next("subject")?,
            })
        })
        .collect()
}

/// Paths the reviewer changed in the worktree and has not committed,
/// including new files.
pub fn uncommitted_paths(workspace_path: &Path) -> Result<Vec<String>, OrbitError> {
    let status = git_output_raw(
        workspace_path,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    let mut paths = status
        .split('\0')
        .filter(|entry| entry.len() > 3)
        .map(|entry| entry[3..].to_string())
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// Commit every uncommitted change as one reviewer-attributed repair commit.
/// Returns `None` when the worktree was clean.
pub fn commit_reviewer_repairs(
    workspace_path: &Path,
    reviewer_model: &str,
    message: &str,
) -> Result<Option<CommitIdentity>, OrbitError> {
    stage_everything(workspace_path)?;
    if staged_paths(workspace_path)?.is_empty() {
        return Ok(None);
    }
    commit_reviewer_repairs_in(workspace_path, reviewer_model, message)?;
    let head = revision(workspace_path, "HEAD")?;
    let mut commits = commits_between(workspace_path, &format!("{}^", head.commit), &head.commit)?;
    commits
        .pop()
        .map(Some)
        .ok_or_else(|| OrbitError::Execution("reviewer repair commit was not recorded".into()))
}

/// What a managed landing looks like against the reviewed candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LandedFacts {
    /// The integration branch immediately before the landing span.
    pub base_at_landing: SourceRevision,
    pub is_candidate_commit: bool,
    pub parents: usize,
    pub span_commits: usize,
}

/// Fetch the landed commit from `origin` so it can be read locally. A
/// failure is reported, never masked: an unreadable landing is uncovered.
pub fn fetch_landed_commit(workspace_path: &Path, landed_commit: &str) -> Result<(), OrbitError> {
    if revision(workspace_path, landed_commit).is_ok() {
        return Ok(());
    }
    git_success(
        workspace_path,
        &["fetch", "--quiet", "origin", landed_commit],
    )
}

/// Read the landing span for a certificate with `candidate_commit_count`
/// commits: a fast-forward keeps the candidate commit, a merge commit has
/// two parents, replayed commits sit on the reviewed base tree, and anything
/// else is one squash commit on its first parent.
pub fn landed_candidate_facts(
    workspace_path: &Path,
    landed: &SourceRevision,
    candidate_commit: &str,
    candidate_base_tree: &str,
    candidate_commit_count: usize,
) -> Result<LandedFacts, OrbitError> {
    let parents = git_output(
        workspace_path,
        &["rev-list", "--parents", "-n", "1", &landed.commit],
    )?
    .split_whitespace()
    .count()
    .saturating_sub(1);
    let is_candidate_commit = landed.commit == candidate_commit;
    let replayed = candidate_commit_count.max(1);
    let base_after_span = format!("{}~{replayed}", landed.commit);
    let span_base = revision(workspace_path, &base_after_span).ok();
    let replayed_on_reviewed_base = (is_candidate_commit || (parents <= 1 && replayed > 1))
        && span_base
            .as_ref()
            .is_some_and(|base| base.tree == candidate_base_tree);
    let (base_at_landing, span_commits) = if replayed_on_reviewed_base {
        (
            span_base.ok_or_else(|| {
                OrbitError::Execution("landing span base vanished while reading it".into())
            })?,
            replayed,
        )
    } else {
        (
            revision(workspace_path, &format!("{}^1", landed.commit))?,
            1,
        )
    };
    Ok(LandedFacts {
        base_at_landing,
        is_candidate_commit,
        parents,
        span_commits,
    })
}
