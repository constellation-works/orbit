pub(crate) mod absorbed;
mod attribution;
mod base;
mod baseline;
mod build_output;
mod candidate_resume;
mod candidate_validate;
mod checks;
pub mod claim;
mod commit;
mod completion_deadline;
pub(crate) mod completion_hold;
mod conventions;
mod deferral_probe;
mod deferred_sandbox;
mod delivery_marker;
mod failure;
mod flaky;
mod forge_text;
mod freshness;
pub(crate) mod git;
mod handoff;
mod host_evidence;
mod interrupted_review;
mod landing;
mod light_delivery;
mod operations;
mod owed_criteria;
mod pr;
mod pr_answers;
mod push;
mod remote_probe;
mod required;
mod resume;
mod retry_lineage;
pub mod review_gate;
mod scratch_target;
mod validation_log;
mod validation_scope;
mod worktree;

#[cfg(test)]
mod tests;

pub use base::claim::{
    ClaimedKnownRed, KnownRedUnconfirmed, verify_claimed_known_red, verify_claimed_known_red_base,
};
pub use baseline::{BaselineHoldStatus, baseline_hold_status, recorded_baseline_hold_status};
pub(super) use candidate_resume::candidate_resume;
pub(super) use candidate_validate::candidate_validate;
pub(super) use claim::{claim_candidate_carry, claim_handoff, claim_validate};
pub(super) use commit::git_commit;
pub(crate) use commit::{
    PUBLISHED_REPLAY_STEPS, published_candidate_matches, require_implementer_no_diff_artifacts,
};
pub(crate) use commit::{
    RecoveryCommit, RecoveryCommitRefusal, RecoveryCommitRequest, commit_recovery_repair,
};
pub use commit::{compose_squash_merge_message, validate_claim_new_paths};
pub use deferral_probe::run_deferral_probe;
pub(crate) use deferred_sandbox::{ImplementerLane, accept_implementer_deferral};
pub(super) use failure::pr_failure_handoff;
pub(crate) use freshness::refreshed_input;
pub(super) use freshness::{prepare_pr_handoff, rebase_pr_branch};
pub use git::fetch_remote_base;
#[cfg(feature = "test-support")]
pub use git::with_worktree_setup_fetch_limits;
pub use landing::landing_wait_gate_cleared;
pub(super) use landing::{handoff_land, pr_checks_watch};
pub use light_delivery::{
    judge_light_delivery, judge_light_delivery_with_key, review_skip_decision,
    run_review_skip_guard,
};
pub(crate) use owed_criteria::accept_implementer_owed_criteria;
pub(crate) use pr::forge_observation_hold_message;
pub(crate) use pr::{
    AUTHENTICATED_PUBLISHED_HEAD_SHA, apply_authenticated_published_head,
    provider_authenticated_published_head, recovery_published_head,
};
pub use pr::{CheckState, PullRequestStatus, classify_rollup_context};
pub(crate) use pr::{CompletionRecoveryChecks, completion_recovery_checks};
pub(super) use pr::{git_merge, pr_complete, pr_open, pr_promote, ship_done_attribution};
pub(super) use push::push_batch_changes;
pub(crate) use remote_probe::fixed_wait;
#[cfg(feature = "test-support")]
pub use remote_probe::{with_remote_probe_backoff, with_vcs_retry_wait};
pub(crate) use resume::reconcile_resumed_failure_handoff;
pub(super) use worktree::setup_worktree;
#[cfg(all(
    any(target_os = "linux", target_os = "macos"),
    feature = "test-support"
))]
pub use worktree::with_host_target_removal_hook;
pub use worktree::{
    WORKTREE_IDENTITY_INPUT_KEYS, WorktreeGcOptions, WorktreeGcRecovery, WorktreeGcResult,
    WorktreeReclaimReport, collect_worktrees, run_worktree_has_reclaim_output, run_worktree_paths,
};

/// Run one engine-private VCS/PR operation against the real `git` and `gh`,
/// outside the public tool registry. A host answers
/// [`crate::RuntimeHost::run_private_vcs_operation`] with it.
pub fn run_private_operation(
    operation: &str,
    input: &serde_json::Value,
) -> Result<serde_json::Value, orbit_common::OrbitError> {
    operations::run(operation, input)
}

/// [ORB-26389] The engine-private operation that merges a pull request. A
/// host answering [`crate::RuntimeHost::run_private_vcs_operation`] audits
/// every request of it, so no Orbit-initiated merge leaves the trail.
pub const PRIVATE_VCS_PR_MERGE: &str = operations::PR_MERGE;

/// The [`PRIVATE_VCS_PR_MERGE`] input field naming the run and step that
/// request the merge and the task it delivers:
/// `{"run_id": …, "step_id": …, "task_id": …}`, each `null` when the
/// caller's input carried none.
pub const PR_MERGE_ATTRIBUTION_FIELD: &str = "attribution";

/// The [`PR_MERGE_ATTRIBUTION_FIELD`] value for a merge requested by the
/// deterministic step whose input is `input`: the dispatcher injects the
/// run as `job_run_id`/`run_id` and the step as `step_id`; the task is the
/// input's `task_id`, else the first of its `task_ids`.
pub(crate) fn pr_merge_attribution(input: &serde_json::Value) -> serde_json::Value {
    let step_id = input
        .get("step_id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|step| !step.is_empty());
    let task_id = input
        .get("task_id")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            input
                .get("task_ids")
                .and_then(serde_json::Value::as_array)
                .and_then(|ids| ids.first())
                .and_then(serde_json::Value::as_str)
        })
        .filter(|id| !id.is_empty());
    serde_json::json!({
        "run_id": super::input::required_job_run_id(input, "pr.merge").ok(),
        "step_id": step_id,
        "task_id": task_id,
    })
}

#[cfg(feature = "test-support")]
pub use completion_deadline::with_prerequisite_clock;
