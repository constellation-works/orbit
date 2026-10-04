mod actions;
mod already_landed;
mod author;
mod checkpoint;
mod diagnostics;
mod git_ops;
mod message;
mod no_diff;
mod repair;
mod scope;
mod summary;

pub(super) use scope::ensure_candidate_ownership;
pub use scope::validate_claim_new_paths;

pub(super) use actions::commit_failure_candidate;
pub(in crate::executor::automation) use actions::git_commit;
pub(super) use checkpoint::{verified_clean_tree_checkpoint, verify_clean_tree_handoff};
pub(super) use repair::{
    commit_reviewer_repairs_in, reviewer_repair_identity, stage_everything, staged_paths,
};

#[cfg(test)]
mod tests;
