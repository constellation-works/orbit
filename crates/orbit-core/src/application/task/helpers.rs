use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::identity::{normalize_attribution_label, normalize_optional_attribution_label};
use orbit_types::task::{Task, TaskComment, TaskStatus};

use crate::context::resolve_write_actor_label;

pub(crate) const SYSTEM_ACTOR_LABEL: &str = "system";

pub(crate) struct TaskAttributionInput<'a> {
    pub(crate) default_actor_label: &'a str,
    pub(crate) actor_override: Option<&'a str>,
    pub(crate) agent: Option<&'a str>,
    pub(crate) model: Option<&'a str>,
    pub(crate) runtime_model_identity: Option<&'a str>,
    pub(crate) plan_changed: bool,
    pub(crate) target_status: Option<TaskStatus>,
    pub(crate) explicit_planned_by: Option<&'a Option<String>>,
    pub(crate) explicit_implemented_by: Option<&'a Option<String>>,
}

pub(crate) struct TaskAttribution {
    pub(crate) actor: String,
    pub(crate) planned_by: Option<Option<String>>,
    pub(crate) implemented_by: Option<Option<String>>,
}

/// Assemble mutation and authored-role attribution for human and automation updates.
///
/// The mutation actor is an explicit override (automation uses `system`) or,
/// otherwise, the shared write-path resolver (canonical family, else the
/// process actor). Explicit authored-role mutations win; inferred role labels
/// use model > agent > runtime model identity > mutation actor, while inferred
/// `implemented_by` preserves an existing value. `implemented_by` inference is
/// applied only while entering review or done.
pub(crate) fn assemble_task_attribution(
    task: &Task,
    input: TaskAttributionInput<'_>,
) -> Result<TaskAttribution, OrbitError> {
    let actor = match input.actor_override {
        Some(label) => normalize_attribution_label(label, None),
        None => effective_actor_label(input.default_actor_label, input.agent, input.model)?,
    };
    let authored_role_label = normalize_optional_attribution_label(input.model, input.model)
        .or_else(|| normalize_optional_attribution_label(input.agent, input.model))
        .or_else(|| normalize_optional_attribution_label(input.runtime_model_identity, None))
        .unwrap_or_else(|| actor.clone());
    let planned_by = input.explicit_planned_by.cloned().or_else(|| {
        input
            .plan_changed
            .then(|| Some(authored_role_label.clone()))
    });
    let implemented_by = input.explicit_implemented_by.cloned().or_else(|| {
        input.target_status.and_then(|status| {
            if matches!(status, TaskStatus::Review | TaskStatus::Done) {
                implementation_label(task, authored_role_label.as_str(), input.model).map(Some)
            } else {
                None
            }
        })
    });

    Ok(TaskAttribution {
        actor,
        planned_by,
        implemented_by,
    })
}

pub(super) fn build_task_comments(
    message: Option<String>,
    by: &str,
) -> Result<Vec<TaskComment>, OrbitError> {
    let Some(message) = message else {
        return Ok(Vec::new());
    };
    let message = message.trim();
    if message.is_empty() {
        return Err(OrbitError::InvalidInput(
            "task comment must not be empty".to_string(),
        ));
    }
    let by = by.trim();
    if by.is_empty() {
        return Err(OrbitError::InvalidInput(
            "task comment author must not be empty".to_string(),
        ));
    }

    Ok(vec![TaskComment {
        at: Utc::now(),
        by: by.to_string(),
        message: message.to_string(),
    }])
}

/// Render an optional task field value for a history note, giving unset values a
/// clear textual marker instead of an empty string (ORB-10311).
pub(super) fn describe_optional_field_value(value: Option<&str>) -> String {
    value
        .map(str::to_string)
        .unwrap_or_else(|| "(none)".to_string())
}

pub(super) fn effective_actor_label(
    default_label: &str,
    agent: Option<&str>,
    model: Option<&str>,
) -> Result<String, OrbitError> {
    resolve_write_actor_label(default_label, agent, model)
}

pub(super) fn implementation_label(
    task: &Task,
    actor_label: &str,
    explicit_model: Option<&str>,
) -> Option<String> {
    if let Some(existing) = task.implemented_by.as_deref() {
        return normalize_optional_attribution_label(Some(existing), None);
    }

    normalize_optional_attribution_label(
        explicit_model.or((!actor_label.trim().is_empty()).then_some(actor_label)),
        explicit_model,
    )
}

pub(super) fn authored_role_value(content: &str, actor_label: &str) -> Option<String> {
    if content.trim().is_empty() {
        None
    } else {
        Some(actor_label.to_string())
    }
}
