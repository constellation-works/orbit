pub(crate) mod absorbed;
mod attribution;
mod base_chase;
mod base_claim;
mod base_obsolescence;
mod base_refresh;
mod baseline;
mod build_output;
mod candidate_resume;
mod candidate_validate;
pub mod claim;
mod commit;
mod completion_deadline;
mod deferral_probe;
mod deferred_sandbox;
mod delivery_marker;
mod failure;
mod flaky;
mod freshness;
pub(crate) mod git;
mod handoff;
mod host_evidence;
mod interrupted_review;
mod landing;
mod landing_checks;
mod operations;
mod pr;
mod push;
mod remote_probe;
mod required_capture;
mod required_command;
mod required_evidence;
mod resume;
pub mod review_gate;
mod scratch_target;
mod worktree;

#[cfg(test)]
mod tests;

pub use base_claim::{ClaimedKnownRed, KnownRedUnconfirmed, verify_claimed_known_red};
pub use baseline::{BaselineHoldStatus, baseline_hold_status, recorded_baseline_hold_status};
pub(super) use candidate_resume::candidate_resume;
pub(super) use candidate_validate::candidate_validate;
pub(super) use claim::{claim_candidate_carry, claim_handoff, claim_validate};
pub(super) use commit::git_commit;
pub(crate) use commit::require_implementer_no_diff_artifacts;
pub use commit::validate_claim_new_paths;
pub(crate) use commit::{
    RecoveryCommit, RecoveryCommitRefusal, RecoveryCommitRequest, commit_recovery_repair,
};
pub use deferral_probe::run_deferral_probe;
pub(crate) use deferred_sandbox::{ImplementerLane, accept_implementer_deferral};
pub(super) use failure::pr_failure_handoff;
pub(crate) use freshness::refreshed_input;
pub(super) use freshness::{prepare_pr_handoff, rebase_pr_branch};
pub use git::fetch_remote_base;
pub(super) use landing::handoff_land;
pub use pr::PullRequestStatus;
pub(super) use pr::{git_merge, pr_complete, pr_open, pr_promote, ship_done_attribution};
pub(super) use push::push_batch_changes;
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
    WorktreeGcOptions, WorktreeGcRecovery, WorktreeGcResult, WorktreeReclaimReport,
    collect_worktrees, run_worktree_has_reclaim_output, run_worktree_paths,
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

#[cfg(feature = "test-support")]
pub use completion_deadline::with_prerequisite_clock;
