//! Proposed-only human capture using the existing screened atomic creation path.

use orbit_common::{
    OrbitError,
    security::{redaction::redact_all, release::sha256_hex},
};
use orbit_types::task::{TaskComplexity, TaskPriority, TaskStatus, TaskType};
use serde::{Deserialize, Serialize};

use super::{TaskAddParams, TaskWriteScreen};
use crate::{ActorIdentity, OrbitRuntime};

/// Strict input for a retryable human proposal. Authority is supplied by the
/// surface, never by these fields. `binding` identifies its frozen destination.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProposedTaskInput {
    pub request_id: String,
    pub binding: String,
    pub title: String,
    pub description: String,
    pub complexity: TaskComplexity,
    pub acceptance_criteria: Vec<String>,
    #[serde(default = "default_priority")]
    pub priority: TaskPriority,
    #[serde(default)]
    pub task_type: Option<TaskType>,
}

fn default_priority() -> TaskPriority {
    TaskPriority::Medium
}

impl OrbitRuntime {
    /// Capture a human proposal after the surface has checked current access
    /// and the exact destination binding. Returns only the immutable task ID;
    /// mutable projections and sidecars cannot change the creation receipt.
    pub fn create_proposed_task(&self, mut input: ProposedTaskInput) -> Result<String, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        input
            .complexity
            .require_assessed()
            .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
        if input.request_id.is_empty()
            || input.request_id.len() > 128
            || !input
                .request_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        {
            return Err(OrbitError::InvalidInput("request_id must contain 1 to 128 ASCII letters, digits, dots, underscores or hyphens".into()));
        }
        if input.binding.len() != 64 || !input.binding.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(OrbitError::InvalidInput(
                "a workspace binding is required".into(),
            ));
        }
        super::desktop::validation::bounded_text(&input.title, true)?;
        super::desktop::validation::bounded_text(&input.description, false)?;
        super::desktop::validation::criteria(&input.acceptance_criteria)?;
        input.title = redact_all(input.title.trim());
        input.description = redact_all(&input.description);
        for criterion in &mut input.acceptance_criteria {
            *criterion = redact_all(criterion.trim());
        }
        // Normalize defaults before digesting, independent of JSON field order.
        input.task_type = Some(input.task_type.unwrap_or(TaskType::Chore));
        let payload = serde_json::to_vec(&input)
            .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
        if payload.len() > 262_144 {
            return Err(OrbitError::InvalidInput("proposal exceeds 256 KiB".into()));
        }
        let digest = sha256_hex(&payload);
        let key = format!("dashboard-create:{}", input.request_id);
        if let Some(id) = self
            .stores()
            .tasks()
            .lookup_committed_creation_id(&key, &digest)?
        {
            return Ok(id);
        }
        let human = self.clone().with_actor(ActorIdentity::human("human"));
        match human.add_task_screened_guarded(
            TaskAddParams {
                title: input.title,
                description: input.description,
                complexity: input.complexity,
                acceptance_criteria: input.acceptance_criteria,
                priority: input.priority,
                task_type: input.task_type,
                status: Some(TaskStatus::Proposed),
                ..Default::default()
            },
            &TaskWriteScreen::unbound(false),
            None,
            None,
            Some(&key),
            Some(&digest),
        ) {
            Ok(written) => {
                for warning in written.warnings {
                    tracing::warn!(task_id = %written.task.id, %warning, "proposal creation advisory");
                }
                Ok(written.task.id)
            }
            // The existing guarded store has committed, even if subsequent
            // provenance/projection work failed. Its receipt remains valid.
            Err(OrbitError::DesktopWriteAccepted { task_id, .. }) => Ok(task_id),
            Err(error) => Err(error),
        }
    }
}
