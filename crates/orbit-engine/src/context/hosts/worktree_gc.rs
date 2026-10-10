//! Owner task lookup results used by worktree garbage collection.

use orbit_types::task::TaskStatus;

/// What worktree GC learned about one task from the store that owns it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeGcTaskLookup {
    /// The owning store answered with the task's current state.
    Found {
        status: TaskStatus,
        pr_status: Option<String>,
    },
    /// The owning store answered but did not produce the task.
    Unresolved,
    /// A replica cannot associate the task's prefix with this machine or its
    /// workspace owner. No owner lookup was attempted; retain the worktree.
    TaskPrefixUnroutable,
    /// This checkout is a replica with no route to ask its owner: it is not
    /// a registered workspace or has no federated destination for the owner.
    /// Nothing was attempted over the wire, so this is a configuration gap,
    /// not an outage. The string names it.
    NoOwnerRoute(String),
    /// The owner route was available, but the owner answered with a failure
    /// while reading the task. The string names the owner's response.
    OwnerLookupFailed(String),
    /// This checkout is a replica and the transport to its owner failed. The
    /// string is the transport's error, reported beside the retained
    /// worktree.
    OwnerUnreachable(String),
}
