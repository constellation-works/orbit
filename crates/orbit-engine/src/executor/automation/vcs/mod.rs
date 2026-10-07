mod attribution;
mod base_chase;
mod base_obsolescence;
mod baseline;
mod candidate_resume;
mod candidate_validate;
pub mod claim;
mod commit;
mod delivery_marker;
mod failure;
mod freshness;
pub(crate) mod git;
mod handoff;
mod landing;
mod operations;
mod pr;
mod push;
mod required_command;
mod resume;
pub mod review_gate;
mod worktree;

#[cfg(test)]
mod tests;

pub use baseline::{BaselineHoldStatus, baseline_hold_status};
pub(super) use candidate_resume::candidate_resume;
pub(super) use candidate_validate::candidate_validate;
pub(super) use claim::{claim_candidate_carry, claim_handoff, claim_validate};
pub(super) use commit::git_commit;
pub use commit::validate_claim_new_paths;
pub(super) use failure::pr_failure_handoff;
pub(super) use freshness::{prepare_pr_handoff, rebase_pr_branch};
pub use git::fetch_remote_base;
pub(super) use landing::handoff_land;
pub(super) use pr::{git_merge, pr_complete, pr_open, pr_promote, ship_done_attribution};
pub(super) use push::push_batch_changes;
pub(crate) use required_command::environment_record;
pub(crate) use resume::reconcile_resumed_failure_handoff;
pub(super) use worktree::setup_worktree;
pub use worktree::{
    WorktreeGcOptions, WorktreeGcResult, collect_worktrees, run_worktree_has_build_output,
};

pub(crate) fn run_private_operation(
    operation: &str,
    input: &serde_json::Value,
) -> Result<serde_json::Value, orbit_common::OrbitError> {
    operations::run(operation, input)
}
