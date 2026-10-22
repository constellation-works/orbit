mod actions;
mod already_landed;
mod author;
mod checkpoint;
mod clean_tree;
mod detached;
mod diagnostics;
mod git_ops;
mod implementer;
mod message;
mod no_diff;
mod published_replay;
mod recovery;
mod repair;
mod scope;
mod summary;
mod tagged_no_diff;

pub(crate) use implementer::require_implementer_no_diff_artifacts;
pub(super) use scope::attribute_candidate_paths;
pub use scope::validate_claim_new_paths;

pub(super) use actions::commit_failure_candidate;
pub(in crate::executor::automation) use actions::git_commit;
pub(super) use checkpoint::{
    claimed_clean_base_checkpoint, verified_clean_tree_checkpoint, verify_clean_tree_handoff,
    verify_clean_tree_handoff_at_revision,
};
pub(super) use detached::{commit_claimed_partial, commit_worktree_detached};
pub(crate) use recovery::{
    RecoveryCommit, RecoveryCommitRefusal, RecoveryCommitRequest, commit_recovery_repair,
};
pub(super) use repair::{
    commit_reviewer_repairs_in, reviewer_repair_identity, stage_everything, staged_paths,
};
pub(super) use tagged_no_diff::DECISION as NO_DIFF_EXPECTED_SKIP;

#[cfg(test)]
mod tests;

pub(crate) use published_replay::{PUBLISHED_REPLAY_STEPS, published_candidate_matches};
