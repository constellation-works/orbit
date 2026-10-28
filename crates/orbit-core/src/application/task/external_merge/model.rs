use orbit_types::task::TaskStatus;

/// The statuses a task settles from.
pub(super) const SETTLEABLE_STATUSES: [TaskStatus; 3] = [
    TaskStatus::Blocked,
    TaskStatus::InProgress,
    TaskStatus::Review,
];

/// A task settled from its merged delivery PR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalMergeSettlement {
    /// The settled task.
    pub task_id: String,
    /// The status the task settled from.
    pub previous_status: TaskStatus,
    /// The merged delivery PR.
    pub pr_number: u64,
    /// The branch it merged into.
    pub base_branch: String,
    /// The commit it merged as.
    pub merge_commit: String,
}

/// A candidate task the batch settlement skipped, with why it did not settle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalMergeSkippedTask {
    /// The skipped task.
    pub task_id: String,
    /// Why it did not settle, without the per-task refusal prefix.
    pub reason: String,
}

/// The outcome of settling every candidate from its own recorded PRs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExternalMergeBatchSettlement {
    /// Tasks settled to `done`, in candidate order.
    pub settled: Vec<ExternalMergeSettlement>,
    /// Candidates left unchanged, in candidate order, with why.
    pub skipped: Vec<ExternalMergeSkippedTask>,
}
