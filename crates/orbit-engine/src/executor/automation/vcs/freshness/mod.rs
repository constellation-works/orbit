//! PR branch freshness, rebasing and host-certified recovery.

mod checkpoint;
mod divergence;
mod rebase;
mod rebase_recovery;

pub(super) use checkpoint::{
    recorded_workspace_matches, recovered_head_checkpoint, recovery_run_id,
};
pub(super) use divergence::{
    BranchFreshness, branch_freshness_against_ref, commit_sha, ensure_branch_fresh_against_base,
    original_base_sha, remote_branch_sha,
};
pub(super) use rebase::{discard_rewrite, perform_rebase_onto_base};
pub(in crate::executor::automation) use rebase::{prepare_pr_handoff, rebase_pr_branch};
pub(super) use rebase_recovery::{
    abort_owned_rebase, ensure_clean_for_rewrite, rebase_belongs_to_attempt,
    rebase_provenance_summary, recovery_pinned_base, unmerged_paths,
};
