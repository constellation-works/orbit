//! Mutation provenance that disqualifies read-only completion continuation.
use crate::task::TaskHistoryEntry;

/// Written before a completion merge, auto-merge or local rebase is attempted.
/// A refused or unknown reply does not erase the attempt.
pub const PR_COMPLETION_MUTATION_STARTED_EVENT: &str = "pr_completion_mutation_started";

/// Whether this task has ever admitted a repair or attempted a completion mutation.
///
/// Deliberately inspect event presence, including malformed or incomplete notes:
/// an unreadable reservation cannot prove absence of a mutation. This narrow
/// continuation supports original publications only; prior task repairs retain
/// their governed recovery route even when they produced the same commit/tree.
pub fn pr_completion_has_mutation_provenance(history: &[TaskHistoryEntry]) -> bool {
    history.iter().any(|entry| {
        matches!(
            entry.event.as_str(),
            super::PR_AUTO_FIX_ADMITTED_EVENT
                | super::PR_AUTO_FIX_PUSHED_EVENT
                | super::PR_BASE_UPDATE_RESERVED_EVENT
                | super::PR_INFRASTRUCTURE_RESERVED_EVENT
                | super::SOURCE_REPAIR_RESERVED
                | super::SOURCE_REPAIR_LINKED
                | PR_COMPLETION_MUTATION_STARTED_EVENT
        )
    })
}
