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

use orbit_store::contracts::{ClaimCandidateRef, ClaimFailure};
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
/// `None` before its commit.
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
        source_run_id: Some(run.run_id.clone()),
        failed_step_id: first_incomplete_step(run, state).map(str::to_string),
        published: false,
        durable_ref: None,
        carry_failure: None,
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
    Some(candidate)
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
    note
}
