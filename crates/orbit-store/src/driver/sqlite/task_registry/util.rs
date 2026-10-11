use std::path::Path;

use chrono::{DateTime, Utc};
use orbit_types::task::{TaskRelationType, TaskStatus};

pub(super) fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

pub(super) fn now_string() -> String {
    Utc::now().to_rfc3339()
}

/// The statuses `TaskStatus::is_terminal` defines as terminal, in the order
/// the index's SQL binds them as parameters.
pub(super) const TERMINAL_STATUSES: [TaskStatus; 3] =
    [TaskStatus::Done, TaskStatus::Archived, TaskStatus::Rejected];

pub(super) fn terminal_month(status: TaskStatus, updated_at: DateTime<Utc>) -> Option<String> {
    status
        .is_terminal()
        .then(|| updated_at.format("%Y-%m").to_string())
}

pub(super) fn relation_type_name(relation_type: TaskRelationType) -> &'static str {
    match relation_type {
        TaskRelationType::BlockedBy => "blocked_by",
        TaskRelationType::ChildOf => "child_of",
        TaskRelationType::SpawnedFrom => "spawned_from",
        TaskRelationType::RegressionFrom => "regression_from",
        TaskRelationType::Supersedes => "supersedes",
        TaskRelationType::RelatedTo => "related_to",
        TaskRelationType::CoveredBy => "covered_by",
        TaskRelationType::Produces => "produces",
        TaskRelationType::Resolves => "resolves",
    }
}

pub(super) fn parse_relation_type_name(raw: &str) -> Result<TaskRelationType, String> {
    match raw {
        "blocked_by" => Ok(TaskRelationType::BlockedBy),
        "child_of" => Ok(TaskRelationType::ChildOf),
        "spawned_from" => Ok(TaskRelationType::SpawnedFrom),
        "regression_from" => Ok(TaskRelationType::RegressionFrom),
        "supersedes" => Ok(TaskRelationType::Supersedes),
        "related_to" => Ok(TaskRelationType::RelatedTo),
        "covered_by" => Ok(TaskRelationType::CoveredBy),
        "produces" => Ok(TaskRelationType::Produces),
        "resolves" => Ok(TaskRelationType::Resolves),
        other => Err(format!("unknown task relation type '{other}'")),
    }
}
