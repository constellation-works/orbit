//! Admission shared by the checkpoint tool and all ordinary task-comment writes.

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::security::redaction::{redact_all, redact_home_dir};
use orbit_types::task::Task;
use orbit_types::workflow::{RESIDENT_TAG, ResidentComment};
use serde_json::{Value, json};

use super::{
    cycle::live_cycle_from_history, resident_mandate_ended, resident_mandate_from_artifact,
};
use crate::OrbitRuntime;
use crate::application::task::TaskUpdateParams;

fn invalid(message: impl Into<String>) -> OrbitError {
    OrbitError::InvalidInput(message.into())
}

/// Refuse sensitive records before any writer can sanitize their verbatim text.
pub(crate) fn ensure_resident_comment_redaction_safe(message: &str) -> Result<(), OrbitError> {
    if !ResidentComment::is_resident_comment(message) {
        return Ok(());
    }
    let record: Value = serde_json::from_str(message)
        .map_err(|error| invalid(format!("resident_record_invalid: {error}")))?;
    // Preserve the serialized-text check, and also inspect decoded strings:
    // JSON escaping and a closing quote must not hide a home path or secret.
    if redact_all(message) != message || resident_record_requires_redaction(&record) {
        return Err(invalid(
            "resident records must be supplied without sensitive text requiring redaction; their quotes are preserved verbatim",
        ));
    }
    Ok(())
}

fn resident_record_requires_redaction(value: &Value) -> bool {
    match value {
        Value::String(text) => redact_all(text) != *text || redact_home_dir(text) != *text,
        Value::Array(items) => items.iter().any(resident_record_requires_redaction),
        Value::Object(fields) => fields.values().any(resident_record_requires_redaction),
        _ => false,
    }
}

impl OrbitRuntime {
    pub(super) fn ensure_resident_comment_assignment(&self, task: &Task) -> Result<(), OrbitError> {
        if !task.tags.iter().any(|tag| tag == RESIDENT_TAG) {
            return Err(invalid(
                "resident records belong on a resident mandate's assignment task",
            ));
        }
        if resident_mandate_ended(task.status) {
            return Err(OrbitError::PolicyDenied(
                "the resident mandate has ended".into(),
            ));
        }
        resident_mandate_from_artifact(self, &task.id)?
            .map_err(|reason| invalid(format!("the mandate is not usable: {reason}")))?;
        Ok(())
    }

    /// Called under the task lock, before any part of a task update writes.
    pub(crate) fn validate_resident_comment_write(
        &self,
        task: &Task,
        message: Option<&str>,
        human_writer: bool,
    ) -> Result<(), OrbitError> {
        let Some(message) = message.filter(|message| ResidentComment::is_resident_comment(message))
        else {
            return Ok(());
        };
        let record = ResidentComment::parse(message).map_err(|error| invalid(error.to_string()))?;
        if record.mandate() != task.id.as_str() {
            return Err(invalid(
                "resident record `mandate` must match the assignment task",
            ));
        }
        self.ensure_resident_comment_assignment(task)?;
        let comments = self.get_task_comments(&task.id)?;
        if let Some(cycle) = record.cycle() {
            let live = live_cycle_from_history(&self.get_task_history(&task.id)?)?;
            let live = live
                .as_ref()
                .filter(|live| live.cycle == cycle && live.expires_at > Utc::now())
                .ok_or_else(|| {
                    OrbitError::PolicyDenied(
                        "resident_cycle_stale: record is not for the mandate's live cycle".into(),
                    )
                })?;
            if let ResidentComment::Checkpoint(checkpoint) = &record {
                if checkpoint.run_id != live.run_id {
                    return Err(OrbitError::PolicyDenied(
                        "resident_cycle_stale: checkpoint run does not own the live cycle".into(),
                    ));
                }
                if comments.iter().any(|comment| matches!(ResidentComment::parse(&comment.message),
                    Ok(ResidentComment::Checkpoint(old)) if old.cycle == cycle && old.mandate == task.id.as_str())) {
                    return Err(OrbitError::PolicyDenied("resident_checkpoint_exists: a cycle writes exactly one checkpoint".into()));
                }
            }
        }
        if let ResidentComment::Answer(answer) = &record
            && !human_writer
            && answer.relayed_by.is_none()
        {
            return Err(invalid("a relayed resident.answer requires `relayed_by`"));
        }
        Ok(())
    }

    /// Append one checkpoint through the same admission as task comments.
    pub fn append_resident_checkpoint(
        &self,
        task_id: &str,
        record: Value,
        agent: Option<String>,
        model: Option<String>,
    ) -> Result<Value, OrbitError> {
        let message = serde_json::to_string(&record)
            .map_err(|error| invalid(format!("serialize checkpoint: {error}")))?;
        if !matches!(
            ResidentComment::parse(&message).map_err(|error| invalid(error.to_string()))?,
            ResidentComment::Checkpoint(_)
        ) {
            return Err(invalid("checkpoint requires a resident.checkpoint record"));
        }
        self.update_task_with_identity(
            task_id,
            TaskUpdateParams {
                comment: Some(message),
                ..Default::default()
            },
            agent,
            model,
        )?;
        Ok(json!({"task_id": task_id, "checkpoint": record}))
    }
}
