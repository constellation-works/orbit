//! Request-scoped task selection and fully validated response rows (ORB-11205).

use orbit_types::task::{
    ArtifactManifestFileV2, ExternalRef, Task, TaskComment, TaskEnvelopeV2, TaskHistoryEntry,
    TaskPriority, TaskRelationType, TaskStatus, TaskType, normalize_task_tags,
};

/// Predicates answered by envelope metadata. `None` statuses means all statuses.
#[derive(Debug, Clone, Default)]
pub struct TaskListFilter {
    /// Continue the canonical created-descending, ID-ascending scan.
    pub scan_before: Option<(chrono::DateTime<chrono::Utc>, String)>,
    /// Case-insensitive ID/title substring matched from envelope metadata.
    pub search: Option<String>,
    pub statuses: Option<Vec<TaskStatus>>,
    pub priority: Option<TaskPriority>,
    pub task_type: Option<TaskType>,
    pub parent_id: Option<String>,
    pub job_run_id: Option<String>,
    pub tags: Vec<String>,
    pub external_ref: Option<ExternalRef>,
    pub has_external_ref_system: Option<String>,
}

impl TaskListFilter {
    pub(crate) fn normalized(&self) -> Self {
        Self {
            tags: normalize_task_tags(self.tags.clone()),
            search: self
                .search
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_lowercase),
            ..self.clone()
        }
    }

    pub(crate) fn matches(&self, task: &TaskEnvelopeV2) -> bool {
        self.scan_before.as_ref().is_none_or(|(at, id)| {
            task.created_at < *at || (task.created_at == *at && task.id > *id)
        }) && self.search.as_ref().is_none_or(|query| {
            task.id.to_lowercase().contains(query) || task.title.to_lowercase().contains(query)
        }) && self
            .statuses
            .as_ref()
            .is_none_or(|values| values.contains(&task.status))
            && self.priority.is_none_or(|value| task.priority == value)
            && self.task_type.is_none_or(|value| task.task_type == value)
            && self.parent_id.as_ref().is_none_or(|value| {
                task.relations.iter().any(|relation| {
                    relation.relation_type == TaskRelationType::ChildOf && relation.target == *value
                })
            })
            && self
                .job_run_id
                .as_ref()
                .is_none_or(|value| task.job_run_id.as_ref() == Some(value))
            && self.tags.iter().all(|tag| {
                task.tags
                    .iter()
                    .any(|available| available.trim().to_lowercase() == *tag)
            })
            && self.external_ref.as_ref().is_none_or(|value| {
                task.external_refs
                    .iter()
                    .any(|candidate| candidate.system == value.system && candidate.id == value.id)
            })
            && self.has_external_ref_system.as_ref().is_none_or(|value| {
                task.external_refs
                    .iter()
                    .any(|candidate| candidate.system == *value)
            })
    }
}

/// Ordered metadata matches. Counts do not certify off-page bundle integrity.
#[derive(Debug, Default)]
pub struct TaskCandidates {
    pub items: Vec<TaskEnvelopeV2>,
    pub total: usize,
}

/// One fully validated bundle, retaining sidecars from that same read.
#[derive(Debug)]
pub struct TaskRow {
    pub task: Task,
    pub comments: Vec<TaskComment>,
    pub history: Vec<TaskHistoryEntry>,
    pub artifacts: Vec<ArtifactManifestFileV2>,
}

#[derive(Debug, Default)]
pub struct TaskPage {
    pub items: Vec<TaskRow>,
    pub total: usize,
    /// Global dependency statuses captured after index freshness/rebuild work.
    pub status_by_id: std::collections::BTreeMap<String, TaskStatus>,
}

/// Residual application predicate; requires full hydration before limiting.
pub type TaskResidualFilter<'a> =
    Option<&'a dyn Fn(&Task, &std::collections::BTreeMap<String, TaskStatus>) -> bool>;
