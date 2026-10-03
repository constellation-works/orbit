//! Guarded desktop task contracts. Caller JSON never supplies actor authority.
use crate::task::{Task, TaskComment, TaskHistoryEntry, TaskPriority};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopTaskRequest {
    pub request_id: String,
    pub operation: DesktopTaskOperation,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DesktopTaskOperation {
    Create {
        title: String,
        description: String,
        acceptance_criteria: Vec<String>,
        #[serde(default = "default_priority")]
        priority: TaskPriority,
        crew: Option<String>,
    },
    Edit {
        id: String,
        expected_revision: String,
        #[serde(default)]
        fields: DesktopTaskFields,
    },
    Comment {
        id: String,
        expected_revision: String,
        comment: String,
    },
    Review {
        id: String,
        expected_revision: String,
        verdict: DesktopReviewVerdict,
        #[serde(default)]
        complete: bool,
    },
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopTaskFields {
    pub title: Option<String>,
    pub description: Option<String>,
    pub acceptance_criteria: Option<Vec<String>>,
    pub priority: Option<TaskPriority>,
    pub crew: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopReviewVerdict {
    pub decision: DesktopReviewDecision,
    pub rationale: String,
    pub criteria: Vec<DesktopCriterionOutcome>,
    pub evidence: Vec<String>,
    pub expected_run_id: Option<String>,
    pub expected_head: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopReviewDecision {
    Accept,
    ChangesRequested,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopCriterionOutcome {
    pub criterion: String,
    pub met: bool,
    pub evidence: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopTaskSnapshot {
    pub schema_version: u32,
    pub observed_at: String,
    pub revision: String,
    pub task: Task,
    pub actions: DesktopTaskActions,
    pub comments: Vec<TaskComment>,
    pub history: Vec<TaskHistoryEntry>,
    pub artifacts: Vec<DesktopArtifactMetadata>,
    pub comments_total: usize,
    pub history_total: usize,
    pub artifacts_total: usize,
    pub reviewed_head: Option<String>,
    pub reviewed_head_reason: Option<String>,
    pub truncated_fields: Vec<String>,
    pub content_truncated: bool,
    pub review: Option<serde_json::Value>,
    pub review_reason: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopTaskActions {
    pub edit: DesktopAction,
    pub comment: DesktopAction,
    pub review: DesktopAction,
    pub complete: DesktopAction,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopAction {
    pub enabled: bool,
    pub reason: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopTaskWriteResult {
    pub snapshot: DesktopTaskSnapshot,
    pub replayed: bool,
}

fn default_priority() -> TaskPriority {
    TaskPriority::Medium
}

/// Registered artifact information exposed without its private blob location.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopArtifactMetadata {
    pub path: String,
    pub media_type: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub created_by: String,
    pub created_at: String,
}
