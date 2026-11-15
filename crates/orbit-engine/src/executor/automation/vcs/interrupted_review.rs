//! Keep a reviewer's uncommitted repair when its run is stopped mid-review
//! [ORB-15314].
//!
//! A run cancelled or interrupted while a review gate's reviewer is working
//! leaves the reviewer's edits uncommitted in the run's worktree, where no
//! later run looks. [`preserve_interrupted_review`] commits them as one
//! reviewer-authored partial-repair commit on top of the candidate head —
//! the subject and trailers a timed-out review's preservation uses, plus the
//! step and attempt — and points [`interrupted_review_ref`] at it. The
//! candidate's branch, index and files are left as they were, so the
//! published branch and any pull request on it are unaffected.
//! [`carry_interrupted_review`] pushes that ref to `origin`, where a run on
//! any host can fetch it; the candidate head is the commit's parent.
//!
//! The run that resumes the candidate squash-merges the repair commit instead
//! of the head, after [`locate_review_repair`] proves it is that run's
//! repair on that head.

use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_types::workflow::ReviewGateAdmissionOutput;
use serde_json::Value;

use crate::context::RuntimeHost;

use super::claim::push_candidate_ref;
use super::commit::commit_worktree_detached;
use super::git::{git_command_success, git_output, git_output_raw, git_output_with_env, git_run};
use super::operations::{CANDIDATE_REF_PREFIX, valid_candidate_ref};
use super::review_gate::{
    PARTIAL_REPAIR_RUN_TRAILER, PARTIAL_REPAIR_SUBJECT, REVIEW_ATTEMPT_TRAILER,
};

/// The pipeline-state key under which the host that stopped a run records
/// what [`preserve_interrupted_review`] kept: `run_id`, `step_id`, `attempt_id`,
/// `repair_ref` and `repair_commit` (null when nothing was kept), whether it
/// was `carried` to `origin`, and `carry_failure` or `failure` when not.
pub const INTERRUPTED_REVIEW_KEY: &str = "interrupted_review";

/// Each review gate's admission, reviewer and settlement steps, in pipeline
/// order. A review is in flight between an admission that applied and its
/// settlement.
const REVIEW_GATES: [(&str, &str, &str); 4] = [
    ("review_gate_admit", "review", "review_gate_settle"),
    (
        "landing_review_gate_admit",
        "landing_review",
        "landing_review_gate_settle",
    ),
    ("re_review_gate_admit", "re_review", "re_review_gate_settle"),
    (
        "re_review_gate_admit_2",
        "re_review_2",
        "re_review_gate_settle_2",
    ),
];

/// A review gate a run stopped inside: admitted, not settled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewInFlight {
    /// The reviewer step.
    pub step_id: String,
    /// The attempt its admission reserved.
    pub attempt_id: String,
    /// The admitted reviewer as `provider / model`, which authors the repair
    /// commit; Orbit authors it when the admission named none.
    pub reviewer_model: Option<String>,
}

/// What [`preserve_interrupted_review`] kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreservedReviewRepair {
    /// The candidate-namespace ref that names the repair commit.
    pub repair_ref: String,
    /// The repair commit; `None` when the reviewer left nothing uncommitted.
    pub commit: Option<String>,
}

/// The pipeline-state key under which the host that stopped a run records
/// what [`preserve_rescue_candidate`] kept: `run_id`, `rescue_ref` and
/// `rescue_commit` (null when nothing was kept), `partial_commit` when
/// uncommitted changes were committed, and `failure` when keeping failed.
pub const RESCUE_CANDIDATE_KEY: &str = "rescue_candidate";

/// What [`preserve_rescue_candidate`] kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreservedRescueCandidate {
    /// The ref that names the kept candidate commit.
    pub rescue_ref: String,
    /// The kept commit: the partial commit when the worktree was dirty,
    /// otherwise the worktree's `HEAD`.
    pub commit: String,
    /// The commit of uncommitted changes, when the worktree was dirty.
    pub partial_commit: Option<String>,
}

/// The ref a stopped run's candidate is kept under, for `run_id`.
pub fn rescue_candidate_ref(run_id: &str) -> Result<String, OrbitError> {
    let plain = !run_id.is_empty()
        && !run_id.starts_with('.')
        && !run_id.ends_with(".lock")
        && !run_id.contains("..")
        && run_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
    if !plain {
        return Err(OrbitError::InvalidInput(format!(
            "rescue candidate: run '{run_id}' does not name a plain ref"
        )));
    }
    Ok(format!("refs/orbit/rescue/{run_id}"))
}

/// Scratch index for the working-tree snapshot. Distinct from the detached
/// commit's index so a refused snapshot cannot collide with it.
const RESCUE_SNAPSHOT_INDEX: &str = "orbit-rescue-snapshot.index";

/// Keep `workspace_path`'s candidate — its `HEAD` plus every uncommitted
/// change, as one detached commit when dirty — under
/// [`rescue_candidate_ref`] in the local repository, before the worktree
/// is torn down. The candidate's branch, index and files are left as they
/// were. The ref keeps the commit reachable after the run branch and
/// worktree are removed.
///
/// A path whose real index differs from both `HEAD` and that working-tree
/// commit cannot be kept in one snapshot. Preservation fails before any ref
/// is written, leaving both versions in the worktree. A clean tree, a change
/// that is only unstaged, and a staged change that matches the working tree
/// still keep the single documented commit (or `HEAD` when nothing differs).
pub fn preserve_rescue_candidate(
    workspace_path: &Path,
    run_id: &str,
) -> Result<PreservedRescueCandidate, OrbitError> {
    let rescue_ref = rescue_candidate_ref(run_id)?;
    let head = git_output(workspace_path, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    refuse_distinct_staged_candidate(workspace_path, &head)?;
    let message = format!(
        "rescue: candidate preserved when its worker stopped\n\nOrbit-Rescue-Run: {run_id}"
    );
    let partial_commit = commit_worktree_detached(workspace_path, None, &message)?;
    let commit = partial_commit.clone().unwrap_or(head);
    git_output(workspace_path, &["update-ref", &rescue_ref, &commit])?;
    Ok(PreservedRescueCandidate {
        rescue_ref,
        commit,
        partial_commit,
    })
}

/// Refuse when the real index holds a version of a path the working-tree
/// snapshot does not. Worktree GC refuses this same ambiguity: one tree
/// cannot be the recovery of both versions, and a ref that named only the
/// working tree would drop the staged one.
fn refuse_distinct_staged_candidate(workspace_path: &Path, head: &str) -> Result<(), OrbitError> {
    // The same comparison worktree GC uses. Skip the snapshot when the index
    // matches HEAD: an ordinary unstaged edit has no unique staged state.
    let staged = super::worktree::index_changed_paths(workspace_path, head)?;
    if staged.is_empty() {
        return Ok(());
    }
    let tree = working_candidate_tree(workspace_path)?;
    if !super::worktree::snapshot_drops_staged(workspace_path, &staged, &tree)? {
        return Ok(());
    }
    Err(OrbitError::Execution(
        "staged paths differ from the working candidate; a single commit cannot preserve both \
         versions, so no rescue ref was written and the branch, index and files are unchanged"
            .to_string(),
    ))
}

/// The tree `commit_worktree_detached` would commit: `HEAD` with the
/// working tree overlaid in a scratch index. The real index is not read or
/// written.
fn working_candidate_tree(workspace_path: &Path) -> Result<String, OrbitError> {
    let index = rescue_snapshot_index(workspace_path)?;
    let index_env = index.to_string_lossy().into_owned();
    let staged = (|| {
        let scratch = [("GIT_INDEX_FILE", index_env.as_str())];
        git_output_with_env(workspace_path, &["read-tree", "HEAD"], &scratch)?;
        git_output_with_env(workspace_path, &["add", "--all", "--", "."], &scratch)?;
        git_output_with_env(workspace_path, &["write-tree"], &scratch)
    })();
    if let Err(error) = std::fs::remove_file(&index)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(index = %index.display(), %error, "rescue snapshot index left behind");
    }
    staged
}

/// Absolute: `--git-path` answers relative to the worktree when its Git
/// directory is inside it.
fn rescue_snapshot_index(workspace_path: &Path) -> Result<PathBuf, OrbitError> {
    let path = PathBuf::from(git_output(
        workspace_path,
        &["rev-parse", "--git-path", RESCUE_SNAPSHOT_INDEX],
    )?);
    Ok(if path.is_absolute() {
        path
    } else {
        workspace_path.join(path)
    })
}

/// The latest review gate in `pipeline` (a run's pipeline state) whose
/// admission applied and whose settlement has not run.
pub fn review_in_flight(pipeline: &Value) -> Option<ReviewInFlight> {
    let step = |id: &str| pipeline.get(id).filter(|output| !output.is_null());
    REVIEW_GATES
        .iter()
        .rev()
        .find_map(|(admit, review, settle)| {
            if step(settle).is_some() {
                return None;
            }
            let output = ReviewGateAdmissionOutput::from_value(step(admit)?).ok()?;
            let admitted = output.admitted()?;
            let attempt_id = admitted.attempt_id.trim();
            (!attempt_id.is_empty()).then(|| ReviewInFlight {
                step_id: (*review).to_string(),
                attempt_id: attempt_id.to_string(),
                reviewer_model: admitted.reviewer.model_label(),
            })
        })
}

/// The ref a stopped review's repair commit is kept under, for `task_id`'s
/// run `run_id`: beside the ref the run's failure hook carries its candidate
/// to, never the same one.
pub fn interrupted_review_ref(task_id: &str, run_id: &str) -> String {
    format!("{CANDIDATE_REF_PREFIX}{task_id}/{run_id}-review")
}

/// Commit the reviewer's uncommitted edits in `workspace_path` as a partial
/// repair of `review`, detached from the candidate branch, and point
/// [`interrupted_review_ref`] at it in the local repository.
pub fn preserve_interrupted_review(
    workspace_path: &Path,
    task_id: &str,
    run_id: &str,
    review: &ReviewInFlight,
) -> Result<PreservedReviewRepair, OrbitError> {
    let repair_ref = interrupted_review_ref(task_id, run_id);
    if !valid_candidate_ref(&repair_ref) {
        return Err(OrbitError::InvalidInput(format!(
            "interrupted review: task '{task_id}' and run '{run_id}' do not name a plain ref"
        )));
    }
    let message = format!(
        "{PARTIAL_REPAIR_SUBJECT} [{task_id}]\n\n{PARTIAL_REPAIR_RUN_TRAILER} {run_id}\n\
         Orbit-Review-Step: {}\n{REVIEW_ATTEMPT_TRAILER}: {}",
        review.step_id, review.attempt_id
    );
    let commit =
        commit_worktree_detached(workspace_path, review.reviewer_model.as_deref(), &message)?;
    if let Some(commit) = &commit {
        git_output(workspace_path, &["update-ref", &repair_ref, commit])?;
    }
    Ok(PreservedReviewRepair { repair_ref, commit })
}

/// Push a kept repair commit to its ref on `origin`. The error is the
/// bounded push diagnostic.
pub fn carry_interrupted_review<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &Path,
    preserved: &PreservedReviewRepair,
) -> Result<(), String> {
    let Some(commit) = &preserved.commit else {
        return Ok(());
    };
    let branch = git_output(workspace_path, &["rev-parse", "--abbrev-ref", "HEAD"])
        .map_err(|error| error.to_string())?;
    push_candidate_ref(host, workspace_path, &branch, commit, &preserved.repair_ref)
}

/// The repair commit an interrupted review of `source_run_id` kept on
/// `candidate_head`: `repair_commit` when known, else what `repair_ref`
/// names here or, fetched once, on `origin`. `Err` says why there is none
/// to apply: it is missing, or it is not that run's partial repair whose
/// sole parent is the candidate head.
pub(in crate::executor::automation::vcs) fn locate_review_repair(
    workspace_path: &Path,
    candidate_head: &str,
    source_run_id: &str,
    repair_ref: &str,
    repair_commit: Option<&str>,
) -> Result<Result<String, String>, OrbitError> {
    if !valid_candidate_ref(repair_ref) {
        return Ok(Err(format!("'{repair_ref}' is not a candidate ref")));
    }
    let resolve = |spec: &str| -> Result<Option<String>, OrbitError> {
        let object = format!("{spec}^{{commit}}");
        if !git_command_success(workspace_path, &["cat-file", "-e", &object])? {
            return Ok(None);
        }
        Ok(Some(git_output(
            workspace_path,
            &["rev-parse", "--verify", &object],
        )?))
    };
    let local = match repair_commit {
        Some(commit) => resolve(commit)?,
        None => resolve(repair_ref)?,
    };
    let commit = match local {
        Some(commit) => commit,
        None => {
            let refspec = format!("+{repair_ref}:{repair_ref}");
            let _ = git_run(workspace_path, &["fetch", "--no-tags", "origin", &refspec])?;
            match resolve(repair_commit.unwrap_or(repair_ref))? {
                Some(commit) => commit,
                None => {
                    return Ok(Err(format!(
                        "the reviewer's repair at '{repair_ref}' is not in this repository or on \
                         origin"
                    )));
                }
            }
        }
    };
    let lineage = git_output(
        workspace_path,
        &["rev-list", "--parents", "-n", "1", &commit],
    )?;
    let parents = lineage.split_whitespace().skip(1).collect::<Vec<_>>();
    if parents != [candidate_head] {
        return Ok(Err(format!(
            "repair {commit} at '{repair_ref}' is not a single commit on candidate \
             {candidate_head}"
        )));
    }
    let message = git_output_raw(
        workspace_path,
        &["log", "-1", "--format=%B", "--end-of-options", &commit],
    )?;
    let run_trailer = format!("{PARTIAL_REPAIR_RUN_TRAILER} {source_run_id}");
    let ours = message
        .lines()
        .next()
        .is_some_and(|subject| subject.starts_with(PARTIAL_REPAIR_SUBJECT))
        && message.lines().any(|line| line.trim() == run_trailer);
    if !ours {
        return Ok(Err(format!(
            "repair {commit} at '{repair_ref}' is not the partial repair of run '{source_run_id}'"
        )));
    }
    Ok(Ok(commit))
}
