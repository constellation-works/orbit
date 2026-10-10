//! Which workflow-run events build their branch's own commit: the identity CI
//! collection and `orbit-core`'s CI failure filing both use to tie a run to a
//! landing commit and to let a newer green run supersede a red one.
//!
//! One owner, so collection and filing agree on which runs count.

/// Whether a run triggered by `event` checks out the tip of its `head_branch`
/// as of the trigger and reports that commit as its head SHA. A `push`, a
/// `schedule` (always the default branch) and a `workflow_dispatch` do. A
/// `pull_request` or `merge_group` run builds a synthetic merge commit
/// instead.
pub fn is_branch_event(event: &str) -> bool {
    matches!(event, "push" | "schedule" | "workflow_dispatch")
}
