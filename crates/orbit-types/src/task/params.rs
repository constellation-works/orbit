//! Caller-supplied parameters for task creation and update.
//!
//! These live beside the task model rather than in a runtime crate so every
//! layer that mints or edits a task — Core, the automation scheduling domain,
//! transports — names one set of fields.

use crate::identity::OrbitId;
use crate::task::{
    ExternalRef, TaskArtifact, TaskComplexity, TaskPriority, TaskRelation, TaskStatus, TaskType,
};

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
    /// Named crew responsible for orchestration attribution, not execution.
    pub orchestrator: Option<String>,
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
            orchestrator: None,
        }
    }
}

#[derive(Default, Clone)]
pub struct TaskUpdateParams {
    pub title: Option<String>,
    pub description: Option<String>,
    pub acceptance_criteria: Option<Vec<String>>,
    pub dependencies: Option<Vec<OrbitId>>,
    pub relations: Option<Vec<TaskRelation>>,
    pub tags: Option<Vec<String>>,
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
    pub pr_status: Option<Option<String>>,
    pub job_run_id: Option<Option<String>>,
    pub crew: Option<Option<String>>,
    pub orchestrator: Option<Option<String>>,
    pub context_files: Option<Vec<String>>,
    pub upsert_artifacts: Vec<TaskArtifact>,
}
