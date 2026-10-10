//! Committing a step recovery's working-tree repair on the host [ORB-14822].
//!
//! The recovery sandbox mounts the worktree's Git metadata read-only, so a
//! recovery agent can repair files but never commit them. When the recovered
//! step runs after the candidate was committed, Orbit commits the repair as
//! the new candidate head before the post-recovery attempt, which reruns the
//! step on that head. The repair passes the same path checks owner footprint
//! widening applies; a repair that fails one is refused before the index is
//! touched.

use std::path::Path;

use orbit_common::fs::selector::claim_new_path_is_safe;

use super::super::git::git_output;
use super::author::recovery_author;
use super::git_ops::{
    ensure_named_branch, ensure_no_unmerged_changes, git_commit_as, stage_paths,
    staged_changed_files,
};
use super::scope::{NewPathPolicy, irregular_candidate_paths, task_candidate_paths};

/// Prefix of a step failure whose recovery repair Orbit refused to commit.
const RECOVERY_COMMIT_REFUSED: &str = "recovery_commit_refused";

/// The recovered step and what the run already decided about its candidate.
pub(crate) struct RecoveryCommitRequest<'a> {
    pub(crate) workspace_path: &'a Path,
    pub(crate) run_id: &'a str,
    pub(crate) failed_step_id: &'a str,
    pub(crate) recovery_activity: &'a str,
    pub(crate) task_ids: &'a [String],
    /// The commit step skipped a clean, no-diff-expected candidate: this run
    /// has no route that delivers a change.
    pub(crate) no_diff_route: bool,
    /// Heads a before-PR review attempt was admitted on or settled.
    pub(crate) reviewed_heads: &'a [String],
}

/// What the host did with the repair.
#[derive(Debug)]
pub(crate) enum RecoveryCommit {
    /// Recovery left nothing to commit.
    Clean,
    /// The repair is the new candidate head.
    Committed {
        commit_sha: String,
        paths: Vec<String>,
    },
}

/// Why a repair was not committed. Every refusal but `Uncommittable` is
/// decided before anything is staged.
#[derive(Debug)]
pub(crate) enum RecoveryCommitRefusal {
    /// Git or `.orbit` metadata, an environment file, or a malformed path.
    ProtectedPath(Vec<String>),
    /// A symlink or gitlink: footprint widening accepts only regular files.
    OutsideFootprint(Vec<String>),
    /// The run committed to delivering no change.
    NoDiffRoute,
    /// The candidate is the head a before-PR review was admitted on or
    /// settled; a repair commit would bypass that review.
    ReviewedCandidate(String),
    /// The worktree could not be inspected or committed.
    Uncommittable(String),
}

impl RecoveryCommitRefusal {
    /// The stable reason code.
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::ProtectedPath(_) => "protected_path",
            Self::OutsideFootprint(_) => "outside_footprint",
            Self::NoDiffRoute => "no_diff_route",
            Self::ReviewedCandidate(_) => "reviewed_candidate",
            Self::Uncommittable(_) => "uncommittable",
        }
    }
}

impl std::fmt::Display for RecoveryCommitRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{RECOVERY_COMMIT_REFUSED}:{}: ", self.code())?;
        match self {
            Self::ProtectedPath(paths) => write!(
                f,
                "the repair changes protected paths {paths:?}; a recovery may not change Git or \
                 `.orbit` metadata or environment files"
            ),
            Self::OutsideFootprint(paths) => write!(
                f,
                "the repair leaves non-regular files {paths:?}; the task footprint widens only \
                 over regular files, never a symlink or gitlink"
            ),
            Self::NoDiffRoute => f.write_str(
                "the commit step skipped this no-diff-expected candidate, so the run has no \
                 route that delivers the repair",
            ),
            Self::ReviewedCandidate(head) => write!(
                f,
                "candidate {head} is under before-PR review; a repair commit would bypass \
                 that review"
            ),
            Self::Uncommittable(detail) => f.write_str(detail),
        }?;
        f.write_str(". Orbit committed nothing and left the repair in the worktree")
    }
}

/// Commit what recovery left in the worktree as a new candidate head with
/// recovery provenance, or refuse it before any Git mutation.
pub(crate) fn commit_recovery_repair(
    request: &RecoveryCommitRequest<'_>,
) -> Result<RecoveryCommit, RecoveryCommitRefusal> {
    let uncommittable =
        |error: orbit_common::OrbitError| RecoveryCommitRefusal::Uncommittable(error.to_string());
    let workspace_path = request.workspace_path.canonicalize().map_err(|error| {
        RecoveryCommitRefusal::Uncommittable(format!(
            "workspace '{}' is not readable: {error}",
            request.workspace_path.display()
        ))
    })?;
    let paths = task_candidate_paths(&workspace_path, NewPathPolicy::Recovery)
        .map_err(uncommittable)?
        .into_iter()
        .collect::<Vec<_>>();
    if paths.is_empty() {
        return Ok(RecoveryCommit::Clean);
    }
    if request.no_diff_route {
        return Err(RecoveryCommitRefusal::NoDiffRoute);
    }
    let head = git_output(&workspace_path, &["rev-parse", "--verify", "HEAD^{commit}"])
        .map_err(uncommittable)?;
    if request.reviewed_heads.contains(&head) {
        return Err(RecoveryCommitRefusal::ReviewedCandidate(head));
    }
    let protected = paths
        .iter()
        .filter(|path| !claim_new_path_is_safe(path))
        .cloned()
        .collect::<Vec<_>>();
    if !protected.is_empty() {
        return Err(RecoveryCommitRefusal::ProtectedPath(protected));
    }
    let irregular = irregular_candidate_paths(&workspace_path, &paths);
    if !irregular.is_empty() {
        return Err(RecoveryCommitRefusal::OutsideFootprint(irregular));
    }
    ensure_named_branch(&workspace_path).map_err(uncommittable)?;
    ensure_no_unmerged_changes(&workspace_path).map_err(uncommittable)?;

    stage_paths(&workspace_path, &paths).map_err(uncommittable)?;
    let staged = staged_changed_files(&workspace_path).map_err(uncommittable)?;
    if staged.is_empty() {
        return Ok(RecoveryCommit::Clean);
    }
    git_commit_as(&workspace_path, &message(request), &recovery_author()).map_err(uncommittable)?;
    let commit_sha = git_output(&workspace_path, &["rev-parse", "--verify", "HEAD^{commit}"])
        .map_err(uncommittable)?;
    Ok(RecoveryCommit::Committed {
        commit_sha,
        paths: staged,
    })
}

fn message(request: &RecoveryCommitRequest<'_>) -> String {
    let markers = if request.task_ids.is_empty() {
        String::new()
    } else {
        format!(" [{}]", request.task_ids.join(", "))
    };
    format!(
        "fix: repair failed `{step}` step from recovery{markers}\n\n\
         Step recovery repaired the working tree with Git metadata read-only; Orbit \
         committed the repair and retries `{step}` on this commit.\n\n\
         Orbit-Recovery-Run: {run}\n\
         Orbit-Recovery-Step: {step}\n\
         Orbit-Recovery-Activity: {activity}",
        step = request.failed_step_id,
        run = request.run_id,
        activity = request.recovery_activity,
    )
}
