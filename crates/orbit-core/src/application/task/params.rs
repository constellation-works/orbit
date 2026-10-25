use orbit_types::identity::OrbitId;

use super::paths::ContextCreationAuthorization;
use orbit_types::task::{
    ExternalRef, TaskArtifact, TaskComment, TaskComplexity, TaskDecision, TaskHistoryEntry,
    TaskOrigin, TaskPriority, TaskRelation, TaskStatus, TaskType,
};

#[derive(Default, Clone)]
pub(crate) struct TaskRecordUpdateParams {
    pub(crate) artifact_origin: Option<orbit_types::task::ExecutionLocation>,
    pub(crate) job_run_machine: Option<Option<orbit_types::task::ExecutionLocation>>,
    pub(crate) artifact_owner_run_id: Option<String>,
    /// Trusted writer class the update entry point derived; never task input.
    pub(crate) artifact_writer: Option<orbit_types::task::ArtifactWriter>,
    pub(crate) actor: String,
    pub(crate) title: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) acceptance_criteria: Option<Vec<String>>,
    pub(crate) dependencies: Option<Vec<String>>,
    pub(crate) relations: Option<Vec<TaskRelation>>,
    pub(crate) tags: Option<Vec<String>>,
    pub(crate) requires_labels: Option<Vec<String>>,
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
    pub(crate) crew_source: Option<Option<String>>,
    pub(crate) orchestrator: Option<Option<String>>,
    pub(crate) origin: Option<TaskOrigin>,
    pub(crate) status_event: Option<String>,
    pub(crate) status_note: Option<String>,
    pub(crate) append_history: Vec<TaskHistoryEntry>,
    pub(crate) append_comments: Vec<TaskComment>,
    pub(crate) upsert_artifacts: Vec<TaskArtifact>,
    /// Canonical selectors of `context_files` newly authorized for creation.
    pub(crate) context_creation: Vec<String>,
    /// [ORB-11305] Forwarded to the store as a compare-and-set on the task's
    /// persisted status. Setting it does not itself constitute a history
    /// change — it only constrains one.
    pub(crate) expected_status: Option<Vec<TaskStatus>>,
    /// The update entry point authorized this write as a human operator's
    /// comment-only update; the store admits it under an execution claim
    /// only when its sole effect is one comment [ORB-16723].
    pub(crate) operator_comment: bool,
}

impl TaskRecordUpdateParams {
    pub(super) fn has_document_changes(&self) -> bool {
        self.title.is_some()
            || self.description.is_some()
            || self.acceptance_criteria.is_some()
            || self.dependencies.is_some()
            || self.relations.is_some()
            || self.tags.is_some()
            || self.requires_labels.is_some()
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
            || self.crew_source.is_some()
            || self.orchestrator.is_some()
            || self.origin.is_some()
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

#[derive(Clone)]
pub struct TaskAddParams {
    pub parent_id: Option<OrbitId>,
    pub title: String,
    pub description: String,
    pub acceptance_criteria: Vec<String>,
    pub dependencies: Vec<OrbitId>,
    pub relations: Vec<TaskRelation>,
    pub tags: Vec<String>,
    /// Exact canonical tools to add to the selected activity baseline.
    pub required_tools: Vec<String>,
    /// Capability labels a host must all carry to run the task.
    pub requires_labels: Vec<String>,
    pub plan: String,
    pub comment: Option<String>,
    pub context_files: Vec<String>,
    pub priority: TaskPriority,
    /// Required at create time. Human/agent surfaces must pass an assessed
    /// value; automated mint and `Default` use [`TaskComplexity::Unassessed`].
    pub complexity: TaskComplexity,
    pub task_type: Option<TaskType>,
    pub status: Option<TaskStatus>,
    /// When true, the task metadata attributes creation to `system`.
    /// Used for auto-generated tasks such as job failure follow-ups.
    pub system_created: bool,
    pub external_refs: Vec<ExternalRef>,
    pub source_task_id: Option<String>,
    pub crew: Option<String>,
    /// Provenance recorded for `crew` when the system chose it from a
    /// setting rather than the caller naming it; `None` records `explicit`.
    pub crew_source: Option<String>,
    /// Named crew responsible for orchestration attribution, not execution.
    pub orchestrator: Option<String>,
    /// The orchestrator's model when the session runs one other than the
    /// `orchestrator` crew's configured model. Omitted, creation snapshots
    /// the crew's configured model; it requires an orchestrator.
    pub orchestrator_model: Option<String>,
    /// Why the task was filed, as the caller stated it. `None` stays unset
    /// for an interactive filing; a `system_created` filing or a sweep finding
    /// its provenance tag marks records [`TaskOrigin::Automation`] instead.
    pub origin: Option<TaskOrigin>,
    /// Creation intent an operator surface's selector screening established
    /// for `context_files` ([`OrbitRuntime::add_task_screened`]).
    ///
    /// [`OrbitRuntime::add_task_screened`]: crate::OrbitRuntime::add_task_screened
    pub context_creation: ContextCreationAuthorization,
    /// Typed decision recorded on the task's `created` history event.
    pub creation_decision: Option<TaskDecision>,
}

impl Default for TaskAddParams {
    fn default() -> Self {
        Self {
            parent_id: None,
            title: String::new(),
            description: String::new(),
            acceptance_criteria: Vec::new(),
            dependencies: Vec::new(),
            relations: Vec::new(),
            tags: Vec::new(),
            required_tools: Vec::new(),
            requires_labels: Vec::new(),
            plan: String::new(),
            comment: None,
            context_files: Vec::new(),
            priority: TaskPriority::Medium,
            complexity: TaskComplexity::Unassessed,
            task_type: None,
            status: None,
            system_created: false,
            external_refs: Vec::new(),
            source_task_id: None,
            crew: None,
            crew_source: None,
            orchestrator: None,
            orchestrator_model: None,
            origin: None,
            context_creation: ContextCreationAuthorization::default(),
            creation_decision: None,
        }
    }
}

#[derive(Default, Clone)]
pub struct TaskUpdateParams {
    /// Trusted transport context; never populated by task input parsing.
    pub trusted_artifact_origin: Option<orbit_types::task::ExecutionLocation>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub acceptance_criteria: Option<Vec<String>>,
    pub dependencies: Option<Vec<OrbitId>>,
    pub relations: Option<Vec<TaskRelation>>,
    pub tags: Option<Vec<String>>,
    /// Replacement capability labels; an empty list clears the requirement.
    pub requires_labels: Option<Vec<String>>,
    pub plan: Option<String>,
    pub execution_summary: Option<String>,
    pub comment: Option<String>,
    pub status: Option<TaskStatus>,
    /// Replacement dispatch priority. `None` leaves the task's priority
    /// untouched; the record layer has always been able to persist it
    /// (ORB-10648 wired it through so a caller-supplied `priority` is applied
    /// rather than discarded).
    pub priority: Option<TaskPriority>,
    pub complexity: Option<TaskComplexity>,
    pub task_type: Option<TaskType>,
    pub source_task_id: Option<Option<String>>,
    pub planned_by: Option<Option<String>>,
    pub implemented_by: Option<Option<String>>,
    /// Review decision understood by the merge gate, matched without case.
    /// Surrounding whitespace is trimmed; case and alias spelling are preserved.
    /// `Some(None)` or an empty string clears; `None` leaves it unchanged.
    pub pr_status: Option<Option<String>>,
    pub job_run_id: Option<Option<String>>,
    pub crew: Option<Option<String>>,
    pub orchestrator: Option<Option<String>>,
    /// Replacement origin. An origin can be corrected but not cleared.
    pub origin: Option<TaskOrigin>,
    pub context_files: Option<Vec<String>>,
    /// Creation intent an operator surface's selector screening established
    /// for the replacement `context_files`. Ignored without `context_files`.
    pub context_creation: ContextCreationAuthorization,
    pub upsert_artifacts: Vec<TaskArtifact>,
    /// Discard the candidate the task's last failed run preserved, so its
    /// next run implements fresh instead of resuming it [ORB-13985].
    /// Recorded as a task history event; refused while a run owns the task.
    pub discard_candidate: bool,
    /// Allow replacing tags when the replacement list drops a system identity tag (`ci-failure:*`).
    pub allow_drop_system_tags: bool,
}

impl TaskUpdateParams {
    /// Whether the update's only requested effect is appending its comment:
    /// the one shape an execution claim admits from an operator [ORB-16723].
    pub(super) fn is_comment_only(&self) -> bool {
        let Self {
            // Transport context and flags that change nothing on their own.
            trusted_artifact_origin: _,
            context_creation: _,
            allow_drop_system_tags: _,
            title,
            description,
            acceptance_criteria,
            dependencies,
            relations,
            tags,
            requires_labels,
            plan,
            execution_summary,
            comment,
            status,
            priority,
            complexity,
            task_type,
            source_task_id,
            planned_by,
            implemented_by,
            pr_status,
            job_run_id,
            crew,
            orchestrator,
            origin,
            context_files,
            upsert_artifacts,
            discard_candidate,
        } = self;
        comment.is_some()
            && title.is_none()
            && description.is_none()
            && acceptance_criteria.is_none()
            && dependencies.is_none()
            && relations.is_none()
            && tags.is_none()
            && requires_labels.is_none()
            && plan.is_none()
            && execution_summary.is_none()
            && status.is_none()
            && priority.is_none()
            && complexity.is_none()
            && task_type.is_none()
            && source_task_id.is_none()
            && planned_by.is_none()
            && implemented_by.is_none()
            && pr_status.is_none()
            && job_run_id.is_none()
            && crew.is_none()
            && orchestrator.is_none()
            && origin.is_none()
            && context_files.is_none()
            && upsert_artifacts.is_empty()
            && !discard_candidate
    }
}

impl From<TaskUpdateParams> for TaskRecordUpdateParams {
    fn from(p: TaskUpdateParams) -> Self {
        Self {
            artifact_origin: p.trusted_artifact_origin,
            title: p.title,
            description: p.description,
            acceptance_criteria: p.acceptance_criteria,
            dependencies: p.dependencies,
            relations: p.relations,
            tags: p.tags,
            requires_labels: p.requires_labels,
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
            origin: p.origin,
            context_files: p.context_files,
            context_creation: p.context_creation.authorize,
            upsert_artifacts: p.upsert_artifacts,
            ..Default::default()
        }
    }
}
