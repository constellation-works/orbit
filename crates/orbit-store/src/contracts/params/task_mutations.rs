use chrono::{DateTime, Utc};
use orbit_types::identity::OrbitId;
use orbit_types::task::{
    ExternalRef, OrchestratorModelSource, TaskArtifact, TaskComment, TaskComplexity, TaskDecision,
    TaskHistoryEntry, TaskOrigin, TaskPriority, TaskRelation, TaskStatus, TaskType,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskCreateParams {
    pub actor: String,
    pub parent_id: Option<OrbitId>,
    pub title: String,
    pub description: String,
    pub acceptance_criteria: Vec<String>,
    pub dependencies: Vec<OrbitId>,
    pub relations: Vec<TaskRelation>,
    pub tags: Vec<String>,
    pub required_tools: Vec<String>,
    /// Operator-granted tools. A non-empty grant is recorded as a
    /// `granted_tools_changed` history event in the creating bundle commit;
    /// the caller has already authorized it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub granted_tools: Vec<String>,
    /// Capability labels a host must all carry to run the task.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires_labels: Vec<String>,
    /// The instant before which no dispatcher starts the task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_before: Option<DateTime<Utc>>,
    pub plan: String,
    pub execution_summary: String,
    pub context_files: Vec<String>,
    /// Deprecated compatibility metadata retained for callers that still
    /// construct the v2 create contract.
    pub repo_root: Option<String>,
    pub created_by: Option<String>,
    pub planned_by: Option<String>,
    pub implemented_by: Option<String>,
    pub status: TaskStatus,
    pub priority: TaskPriority,
    pub complexity: Option<TaskComplexity>,
    pub task_type: TaskType,
    pub external_refs: Vec<ExternalRef>,
    pub source_task_id: Option<String>,
    pub crew: Option<String>,
    /// Trusted assignment provenance, absent on legacy creation contracts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crew_source: Option<String>,
    pub orchestrator: Option<String>,
    /// The orchestrator's model resolved at creation; never updated later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestrator_model: Option<String>,
    /// Which rule supplied `orchestrator_model`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestrator_model_source: Option<OrchestratorModelSource>,
    /// Why the task was filed; absent on legacy creation contracts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<TaskOrigin>,
    pub comments: Vec<TaskComment>,
    /// Exact canonical `context_files` selectors an operator surface accepted
    /// for creation under `allow_missing_context`. Recorded as the task's
    /// initial creation grant in the same bundle commit as the task.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_creation: Vec<String>,
    /// Typed decision recorded on the task's `created` history event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creation_decision: Option<TaskDecision>,
}

/// Parameters for a partial update to an existing task.
///
/// Fields that are `None` are left unchanged. Fields of type `Option<Option<T>>`
/// follow the "outer = whether to update, inner = new value" convention:
/// - `None`           → leave the field untouched
/// - `Some(Some(v))`  → set the field to `v`
/// - `Some(None)`     → explicitly clear the field (set it to null/absent)
#[derive(Debug, Default, Clone)]
pub struct TaskDocumentUpdateParams {
    pub actor: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub acceptance_criteria: Option<Vec<String>>,
    pub dependencies: Option<Vec<OrbitId>>,
    pub relations: Option<Vec<TaskRelation>>,
    pub tags: Option<Vec<String>>,
    /// Replacement operator-granted tools; an empty list clears the grant.
    /// The caller authorizes the change and records its history entry.
    pub granted_tools: Option<Vec<String>>,
    /// Replacement capability labels; an empty list clears the requirement.
    pub requires_labels: Option<Vec<String>>,
    /// Replacement scheduled start; `Some(None)` clears it.
    pub not_before: Option<Option<DateTime<Utc>>>,
    pub plan: Option<String>,
    pub execution_summary: Option<String>,
    pub context_files: Option<Vec<String>>,
    pub created_by: Option<Option<String>>,
    pub planned_by: Option<Option<String>>,
    pub implemented_by: Option<Option<String>>,
    pub priority: Option<TaskPriority>,
    pub complexity: Option<TaskComplexity>,
    pub task_type: Option<TaskType>,
    pub external_refs: Option<Vec<ExternalRef>>,
    pub pr_status: Option<Option<String>>,
    pub source_task_id: Option<Option<String>>,
    pub job_run_id: Option<Option<String>>,
    /// Trusted link location supplied with the run binding, never tool input.
    pub job_run_machine: Option<Option<orbit_types::task::ExecutionLocation>>,
    pub crew: Option<Option<String>>,
    /// Assignment provenance updated with the selected crew.
    pub crew_source: Option<Option<String>>,
    pub orchestrator: Option<Option<String>>,
    /// Replacement origin. An origin is set, never cleared.
    pub origin: Option<TaskOrigin>,
    /// Exact canonical selectors of the replacement `context_files` an
    /// operator surface newly authorized for creation. The task's creation
    /// grant is recomputed and recorded in the same bundle commit whenever
    /// `context_files` changes: grants for kept selectors survive, these are
    /// added, and grants for removed selectors are revoked.
    pub context_creation: Vec<String>,
}

#[derive(Debug, Default, Clone)]
pub struct TaskHistoryUpdateParams {
    pub actor: String,
    pub status: Option<TaskStatus>,
    pub status_event: Option<String>,
    pub status_note: Option<String>,
    pub append_history: Vec<TaskHistoryEntry>,
    pub append_comments: Vec<TaskComment>,
    /// [ORB-11305] Compare-and-set guard: when `Some`, the write is applied
    /// only if the task's *persisted* status at write time is one of these.
    ///
    /// The check runs inside the per-task exclusive file lock, after the
    /// bundle is re-read, so a status a caller observed before calling cannot
    /// go stale in the gap. `None` keeps the unconditional last-writer-wins
    /// behavior every other caller relies on.
    pub expected_status: Option<Vec<TaskStatus>>,
    /// Set only by `orbit-core` for a write it authorized as a human
    /// operator's. While an execution claim protects the task, the store
    /// admits such a write only when its sole effect is appending exactly one
    /// comment; every other unscoped write stays refused [ORB-16723].
    pub operator_comment: bool,
}

/// One task-bundle mutation whose freshness guard, durable receipt, history
/// event, and envelope changes share the envelope publish commit point.
#[derive(Debug, Clone)]
pub struct AtomicTaskMutationParams {
    pub actor: String,
    pub operation_id: String,
    pub expected_context_files: Vec<String>,
    pub expected_status: TaskStatus,
    pub expected_complexity: Option<TaskComplexity>,
    /// Crew observed under the application task lock, guarded at publication.
    pub expected_crew: Option<String>,
    pub expected_crew_source: Option<String>,
    /// Replacement selection, committed with the complexity and history.
    pub crew: Option<String>,
    pub crew_source: Option<String>,
    /// Identity of the creation grant the caller validated against
    /// (`ContextCreationState::identity`); a different grant at the write
    /// boundary makes the mutation stale.
    pub expected_context_creation: Option<String>,
    pub context_files: Vec<String>,
    /// Tags appended to the task's own in the same commit, after the usual
    /// normalization and `os:` namespace check. Existing tags are never
    /// removed or replaced; empty adds none.
    pub add_tags: Vec<String>,
    /// Validated dependency targets appended atomically with the assessment.
    /// Existing relations are preserved and repeated targets add no edge.
    pub add_dependencies: Vec<String>,
    pub status: TaskStatus,
    pub complexity: TaskComplexity,
    pub event_type: String,
    pub event_note: String,
    /// Bounded, single-line description stored with the operation receipt in history.
    pub history_summary: String,
    /// Full decision evidence stored as a task comment in the same bundle commit.
    pub audit_note: String,
    /// Additional decision records committed before the operation receipt.
    pub append_history: Vec<TaskHistoryEntry>,
    /// Typed decision recorded on the operation's own history event.
    pub decision: Option<TaskDecision>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtomicTaskMutationOutcome {
    Applied,
    AlreadyApplied,
    Stale,
}

#[derive(Debug, Default, Clone)]
pub struct TaskArtifactUpdateParams {
    /// Owner-supplied authenticated put identity. Legacy/unknown remains None.
    pub origin: Option<orbit_types::task::ExecutionLocation>,
    pub actor: String,
    /// Trusted executor context supplied by Core, never parsed from tool input.
    pub owner_run_id: Option<String>,
    /// Trusted writer class Core derives from the write path, never parsed
    /// from tool input. `None` records an agent or unclassified writer.
    pub writer: Option<orbit_types::task::ArtifactWriter>,
    /// Artifact files to write under the task bundle `artifacts/` directory.
    /// Existing files at the same relative path are overwritten.
    pub upsert_artifacts: Vec<TaskArtifact>,
}
