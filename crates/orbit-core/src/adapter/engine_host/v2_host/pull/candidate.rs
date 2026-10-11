//! The committed candidate a claimed leaf ended with, as its failure
//! settlement names it for the owner to keep [ORB-14257], and where the task's
//! next claim can fetch it [ORB-14338].
//!
//! A claimed PR leaf's candidate is the branch it pushed or, before its push,
//! the branch it synchronized or prepared; its failure hook
//! (`claim_candidate_carry`) pushes an unpublished one to a durable ref on
//! `origin` and records the branch tip it carried, or why it could not. A
//! claimed-local leaf commits on its worktree branch in the owner's own
//! repository, which every later claim of the task shares, so its commit
//! checkpoint names the candidate.
//!
//! [ORB-15314] A leaf stopped inside a review gate also names where the host
//! that stopped it kept the reviewer's uncommitted repair, which the owner
//! completes with the attempt's findings.

use orbit_engine::review_gate::{INTERRUPTED_REVIEW_KEY, interrupted_review_ref, review_in_flight};
use orbit_store::contracts::{ClaimCandidateRef, ClaimFailure, InterruptedReview};
use orbit_types::workflow::{JobRun, PipelineState};

/// The claimed PR leaf's base synchronization step.
pub(super) const SYNC_BASE_STEP: &str = "sync_base";

/// The claimed PR leaf's failure hook, whose checkpointed output says where
/// it carried an unpublished candidate.
const CANDIDATE_CARRY_ACTIVITY: &str = "claim_candidate_carry";

/// The claimed owner-local leaf definition.
const CLAIMED_LOCAL_PIPELINE: &str = "task_claimed_local_pipeline";

/// The claimed PR leaf's steps in order, from the commit on: a step a leaf's
/// pipeline state holds no output for did not complete. `review`,
/// `landing_review` and `landing_push` are absent because they are skipped
/// when no review, or no reviewer fix, applies.
const CLAIMED_PR_DELIVERY_STEPS: [&str; 12] = [
    "commit",
    "prepare_branch",
    SYNC_BASE_STEP,
    "review_gate_admit",
    "review_gate_settle",
    "validate",
    "push",
    "pr_open",
    "landing_review_gate_admit",
    "landing_review_gate_settle",
    "landing_review_validate",
    "pin_validation",
];

/// The claimed-local leaf's steps in order, from the commit on.
const CLAIMED_LOCAL_DELIVERY_STEPS: [&str; 2] = ["commit", "validate"];

/// The first delivery step the leaf did not complete, from its pipeline
/// state; `None` when it stopped before its commit or after them all.
/// A partial candidate records its failed step in the failure checkpoint.
pub(super) fn first_incomplete_step(run: &JobRun, state: &PipelineState) -> Option<&'static str> {
    let done = |step: &str| {
        state
            .pipeline
            .get(step)
            .is_some_and(|output| !output.is_null())
    };
    if !done("commit") {
        return None;
    }
    let steps: &[&'static str] = if run.job_id == CLAIMED_LOCAL_PIPELINE {
        &CLAIMED_LOCAL_DELIVERY_STEPS
    } else {
        &CLAIMED_PR_DELIVERY_STEPS
    };
    steps.iter().copied().find(|step| !done(step))
}

/// The committed candidate the leaf ended with, from its pipeline state.
/// Before its normal commit, only a partial candidate preserved by the
/// failure hook qualifies.
///
/// A PR leaf's is the branch it pushed — last by a before-landing reviewer's
/// fix — with the pull request it opened for it, or the one an earlier claim
/// opened on the branch it continued [ORB-15308]. Before its push, it is the branch tip its failure hook carried to a
/// durable ref — or could not, and why — else the branch it synchronized
/// onto the base, or, when synchronization itself stopped it, the branch it
/// prepared. A claimed-local leaf's is its worktree branch at the commit it
/// made [ORB-14338].
pub(super) fn preserved_candidate(
    run: &JobRun,
    state: &PipelineState,
) -> Option<ClaimCandidateRef> {
    let text = |output: Option<&serde_json::Value>, field: &str| {
        output
            .and_then(|output| output.get(field))
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let step = |step: &str| state.pipeline.get(step);
    let both = |output: Option<&serde_json::Value>, branch: &str, head: &str| {
        Some((text(output, branch)?, text(output, head)?))
    };
    // [ORB-15308] A leaf that continued an earlier claim's branch carries
    // that claim's pull request until its own `pr_open` reuses it, so the
    // next claim still reuses or supersedes it.
    let continued = text(step("resume_candidate"), "reused_branch")
        .and_then(|_| text(step("resume_candidate"), "prior_pull_request"));
    let mut candidate = ClaimCandidateRef {
        branch: String::new(),
        head_sha: String::new(),
        pull_request: text(step("pr_open"), "pr_number").or(continued),
        pull_request_head: None,
        source_run_id: Some(run.run_id.clone()),
        failed_step_id: first_incomplete_step(run, state).map(str::to_string),
        published: false,
        durable_ref: None,
        carry_failure: None,
        interrupted_review: kept_interrupted_review(state),
    };
    let committed = text(step("commit"), "commit_sha");
    if run.job_id == CLAIMED_LOCAL_PIPELINE {
        (candidate.branch, candidate.head_sha) = (text(step("worktree"), "head_ref")?, committed?);
        return Some(candidate);
    }
    // [ORB-14849] A before-landing reviewer's fix, pushed under a lease, is
    // the published head from then on.
    if let Some(pushed) = both(step("landing_push"), "branch", "local_sha")
        .or_else(|| both(step("push"), "branch", "local_sha"))
    {
        (candidate.branch, candidate.head_sha) = pushed;
        candidate.published = true;
        return Some(candidate);
    }
    let carry = state
        .failure_activity_checkpoint
        .as_ref()
        .filter(|checkpoint| checkpoint.activity_name == CANDIDATE_CARRY_ACTIVITY)
        .map(|checkpoint| &checkpoint.output);
    if carry
        .and_then(|carry| carry.get("partial"))
        .and_then(serde_json::Value::as_bool)
        == Some(true)
    {
        candidate.failed_step_id = state
            .failure_activity_checkpoint
            .as_ref()
            .map(|checkpoint| checkpoint.failed_step_id.clone());
    }
    (candidate.branch, candidate.head_sha) = both(carry, "branch", "head_sha")
        .or_else(|| both(step(SYNC_BASE_STEP), "head", "head_sha"))
        .or_else(|| both(step("prepare_branch"), "head", "head_sha"))?;
    match text(carry, "carry").as_deref() {
        Some("durable") => candidate.durable_ref = text(carry, "durable_ref"),
        Some("published") => candidate.published = true,
        Some("failed") => {
            candidate.carry_failure =
                Some(text(carry, "reason").unwrap_or_else(|| "the push failed".to_string()));
        }
        _ => {}
    }
    // [ORB-15314] A repair carried to `origin` has the candidate head as its
    // parent, so its ref serves the head to any host as well.
    let kept = step(INTERRUPTED_REVIEW_KEY);
    if candidate.durable_ref.is_none()
        && !candidate.published
        && kept.and_then(|kept| kept.get("carried")) == Some(&serde_json::Value::Bool(true))
    {
        candidate.durable_ref = text(kept, "repair_ref");
    }
    Some(candidate)
}

/// [`preserved_candidate`] for a leaf about to be stopped: a review it is
/// inside names the ref its stop keeps the reviewer's repair under, which the
/// next claim resolves and checks before applying it.
pub(super) fn stopping_candidate(run: &JobRun, state: &PipelineState) -> Option<ClaimCandidateRef> {
    let mut candidate = preserved_candidate(run, state)?;
    if candidate.interrupted_review.is_none()
        && let Some(review) = review_in_flight(&state.pipeline)
        && let Some(serde_json::Value::Array(task_ids)) =
            run.input.as_ref().and_then(|input| input.get("task_ids"))
        && let [serde_json::Value::String(task_id)] = task_ids.as_slice()
    {
        candidate.interrupted_review = Some(InterruptedReview {
            step_id: review.step_id,
            attempt_id: review.attempt_id,
            repair_ref: Some(interrupted_review_ref(task_id, &run.run_id)),
            repair_commit: None,
            verdict: None,
            findings: Vec::new(),
        });
    }
    Some(candidate)
}

/// What the host that stopped the leaf kept of the review it was inside.
fn kept_interrupted_review(state: &PipelineState) -> Option<InterruptedReview> {
    let kept = state
        .pipeline
        .get(INTERRUPTED_REVIEW_KEY)
        .filter(|kept| kept.is_object())?;
    let text = |field: &str| {
        kept.get(field)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let repair_commit = text("repair_commit");
    Some(InterruptedReview {
        step_id: text("step_id")?,
        attempt_id: text("attempt_id")?,
        repair_ref: repair_commit.as_ref().and(text("repair_ref")),
        repair_commit,
        verdict: None,
        findings: Vec::new(),
    })
}

/// Where the failure's candidate is kept for the next claim, if it has one.
pub(super) fn candidate_note(failure: &ClaimFailure) -> String {
    let Some(candidate) = &failure.candidate else {
        return String::new();
    };
    let mut note = format!(
        "; its candidate is preserved as `{}` at {}",
        candidate.branch, candidate.head_sha,
    );
    if let Some(pull_request) = &candidate.pull_request {
        note.push_str(&format!(" (pull request #{pull_request})"));
    }
    if candidate.published {
        note.push_str(", and the task's next claim resumes it");
    } else if let Some(reference) = &candidate.durable_ref {
        note.push_str(&format!(
            ", carried to `{reference}` on origin, and the task's next claim resumes it on any \
             host"
        ));
    } else {
        note.push_str(" on this host only");
        if let Some(carry_failure) = &candidate.carry_failure {
            note.push_str(&format!(
                " (it could not be pushed to a durable ref: {carry_failure})"
            ));
        }
        note.push_str(
            ", so the task's next claim resumes it only here and a claim on another host \
             implements fresh",
        );
    }
    if let Some(review) = &candidate.interrupted_review {
        note.push_str(&format!(
            "; it was stopped during review step `{}` (attempt `{}`)",
            review.step_id, review.attempt_id
        ));
        match &review.repair_ref {
            Some(reference) => note.push_str(&format!(
                ", and the reviewer's uncommitted repair is kept at `{reference}` for that claim \
                 to continue from with the attempt's findings"
            )),
            None => note.push_str(", and the reviewer left no uncommitted repair"),
        }
    }
    note
}
