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

use std::path::Path;

use orbit_common::OrbitError;
use serde_json::Value;

use crate::context::RuntimeHost;
use crate::executor::automation::input::input_string_field;

use super::claim::push_candidate_ref;
use super::commit::commit_worktree_detached;
use super::git::{git_command_success, git_output, git_output_raw, git_run};
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

/// The latest review gate in `pipeline` (a run's pipeline state) whose
/// admission applied and whose settlement has not run.
pub fn review_in_flight(pipeline: &Value) -> Option<ReviewInFlight> {
    let step = |id: &str| pipeline.get(id).filter(|output| !output.is_null());
    REVIEW_GATES
        .iter()
        .rev()
        .find_map(|(admit, review, settle)| {
            let admitted = step(admit)?;
            if admitted.get("applies").and_then(Value::as_bool) != Some(true)
                || step(settle).is_some()
            {
                return None;
            }
            let attempt_id = input_string_field(admitted, "attempt_id")?;
            let reviewer = admitted.get("reviewer");
            let field = |name: &str| {
                reviewer
                    .and_then(|reviewer| input_string_field(reviewer, name))
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
            };
            let reviewer_model = field("provider")
                .zip(field("model"))
                .map(|(provider, model)| format!("{provider} / {model}"));
            Some(ReviewInFlight {
                step_id: (*review).to_string(),
                attempt_id,
                reviewer_model,
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
