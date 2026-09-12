//! Core's coordinated record-write parameters.
//!
//! The caller-facing `TaskAddParams` / `TaskUpdateParams` shapes live in
//! `orbit_types::task`; this module owns only the record layer they translate
//! into.

use orbit_types::task::{
    ExternalRef, TaskArtifact, TaskComment, TaskComplexity, TaskHistoryEntry, TaskPriority,
    TaskRelation, TaskStatus, TaskType, TaskUpdateParams,
};

#[derive(Default, Clone)]
pub(crate) struct TaskRecordUpdateParams {
    pub(crate) artifact_owner_run_id: Option<String>,
    pub(crate) actor: String,
    pub(crate) title: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) acceptance_criteria: Option<Vec<String>>,
    pub(crate) dependencies: Option<Vec<String>>,
    pub(crate) relations: Option<Vec<TaskRelation>>,
    pub(crate) tags: Option<Vec<String>>,
    pub(crate) plan: Option<String>,
    pub(crate) execution_summary: Option<String>,
    pub(crate) context_files: Option<Vec<String>>,
    pub(crate) created_by: Option<Option<String>>,
    pub(crate) planned_by: Option<Option<String>>,
    pub(crate) implemented_by: Option<Option<String>>,
    pub(crate) status: Option<TaskStatus>,
    pub(crate) priority: Option<TaskPriority>,
    pub(crate) complexity: Option<TaskComplexity>,
    pub(crate) task_type: Option<TaskType>,
    pub(crate) external_refs: Option<Vec<ExternalRef>>,
    pub(crate) pr_status: Option<Option<String>>,
    pub(crate) source_task_id: Option<Option<String>>,
    pub(crate) job_run_id: Option<Option<String>>,
    pub(crate) crew: Option<Option<String>>,
    pub(crate) orchestrator: Option<Option<String>>,
    pub(crate) status_event: Option<String>,
    pub(crate) status_note: Option<String>,
    pub(crate) append_history: Vec<TaskHistoryEntry>,
    pub(crate) append_comments: Vec<TaskComment>,
    pub(crate) upsert_artifacts: Vec<TaskArtifact>,
    /// [ORB-11305] Forwarded to the store as a compare-and-set on the task's
    /// persisted status. Setting it does not itself constitute a history
    /// change — it only constrains one.
    pub(crate) expected_status: Option<Vec<TaskStatus>>,
}

impl TaskRecordUpdateParams {
    pub(super) fn has_document_changes(&self) -> bool {
        self.title.is_some()
            || self.description.is_some()
            || self.acceptance_criteria.is_some()
            || self.dependencies.is_some()
            || self.relations.is_some()
            || self.tags.is_some()
            || self.plan.is_some()
            || self.execution_summary.is_some()
            || self.context_files.is_some()
            || self.created_by.is_some()
            || self.planned_by.is_some()
            || self.implemented_by.is_some()
            || self.priority.is_some()
            || self.complexity.is_some()
            || self.task_type.is_some()
            || self.external_refs.is_some()
            || self.pr_status.is_some()
            || self.source_task_id.is_some()
            || self.job_run_id.is_some()
            || self.crew.is_some()
            || self.orchestrator.is_some()
    }

    pub(super) fn has_history_changes(&self) -> bool {
        self.status.is_some()
            || self.status_event.is_some()
            || self.status_note.is_some()
            || !self.append_history.is_empty()
            || !self.append_comments.is_empty()
    }

    pub(super) fn has_artifact_changes(&self) -> bool {
        !self.upsert_artifacts.is_empty()
    }
}

impl From<TaskUpdateParams> for TaskRecordUpdateParams {
    fn from(p: TaskUpdateParams) -> Self {
        Self {
            title: p.title,
            description: p.description,
            acceptance_criteria: p.acceptance_criteria,
            dependencies: p.dependencies,
            relations: p.relations,
            tags: p.tags,
            plan: p.plan,
            execution_summary: p.execution_summary,
            status: p.status,
            priority: p.priority,
            complexity: p.complexity,
            task_type: p.task_type,
            source_task_id: p.source_task_id,
            planned_by: p.planned_by,
            implemented_by: p.implemented_by,
            pr_status: p.pr_status,
            job_run_id: p.job_run_id,
            crew: p.crew,
            orchestrator: p.orchestrator,
            context_files: p.context_files,
            upsert_artifacts: p.upsert_artifacts,
            ..Default::default()
        }
    }
}
