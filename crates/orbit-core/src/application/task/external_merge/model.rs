use crate::application::task::NativePublicationIdentity;
use crate::runtime::task_pr_forge::MergedPullRequest;
use orbit_types::task::TaskStatus;
use serde_json::{Value, json};

/// The statuses a task settles from.
pub(super) const SETTLEABLE_STATUSES: [TaskStatus; 4] = [
    TaskStatus::Blocked,
    TaskStatus::Backlog,
    TaskStatus::InProgress,
    TaskStatus::Review,
];

/// The single PR a requeue preserved for a backlog task.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum BacklogDelivery {
    FailureHandoff(u64),
    OwnerPublication(u64),
}

impl NativePublicationIdentity {
    pub(super) fn matches_exact_merge(&self, merged: &MergedPullRequest) -> bool {
        self.number.parse::<u64>().ok() == Some(merged.pull.number)
            && self.url.as_deref() == Some(merged.url.as_str())
            && self.branch == merged.pull.head_branch
            && self.base_branch == merged.base_branch
            && self.head == merged.head_sha
    }
}

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

/// Which existing settlement-row contract a surface serves.
///
/// The CLI row carries the five settlement fields; the HTTP row carries those
/// five plus `status: "done"`. Each surface keeps its own envelope (the CLI
/// always serves `{settled: [...]}`, HTTP serves a bare row for one task);
/// the profile only selects the row shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalMergeSettlementProfile {
    /// `orbit task settle-merged`: the five settlement fields, no `status`.
    Cli,
    /// Dashboard HTTP row: the five fields plus `status`.
    Http,
}

impl ExternalMergeSettlement {
    /// The shared settlement-row projection every surface serves.
    ///
    /// The [`ExternalMergeSettlementProfile::Cli`] row is the five settlement
    /// fields; [`ExternalMergeSettlementProfile::Http`] adds `status: "done"`.
    /// No renames or formatting changes: the field mapping is the one the CLI
    /// and HTTP paths each carried before.
    pub fn settlement_json(&self, profile: ExternalMergeSettlementProfile) -> Value {
        match profile {
            ExternalMergeSettlementProfile::Cli => json!({
                "task_id": self.task_id,
                "previous_status": self.previous_status,
                "pr_number": self.pr_number,
                "base_branch": self.base_branch,
                "merge_commit": self.merge_commit,
            }),
            ExternalMergeSettlementProfile::Http => json!({
                "task_id": self.task_id,
                "previous_status": self.previous_status,
                "status": "done",
                "pr_number": self.pr_number,
                "base_branch": self.base_branch,
                "merge_commit": self.merge_commit,
            }),
        }
    }
}
