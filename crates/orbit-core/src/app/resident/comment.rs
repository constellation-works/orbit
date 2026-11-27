//! Admission shared by the checkpoint tool and all ordinary task-comment writes.

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::security::redaction::{redact_all, redact_home_dir};
use orbit_types::task::Task;
use orbit_types::workflow::{
    PipelineState, RESIDENT_CYCLE_JOB, RESIDENT_TAG, ResidentCheckpointRefusal, ResidentComment,
    RunStateUpdate,
};
use serde_json::{Value, json};
use std::ffi::OsStr;

use super::cycle::live_cycle_from_history;
use super::due::checkpoints;
use super::{resident_mandate_ended, resident_mandate_from_artifact};
use crate::OrbitRuntime;
use crate::application::task::TaskUpdateParams;

const MAX_CHECKPOINT_REFUSAL_CHARS: usize = 500;

pub(super) fn ensure_resident_comment_assignment(
    rt: &OrbitRuntime,
    task: &Task,
) -> Result<(), OrbitError> {
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
    resident_mandate_from_artifact(rt, &task.id)?
        .map_err(|reason| invalid(format!("the mandate is not usable: {reason}")))?;
    Ok(())
}

/// Called under the task lock, before any part of a task update writes.
pub(crate) fn validate_resident_comment_write(
    rt: &OrbitRuntime,
    task: &Task,
    message: Option<&str>,
    human_writer: bool,
) -> Result<(), OrbitError> {
    let Some(message) = message.filter(|message| ResidentComment::is_resident_comment(message))
    else {
        return Ok(());
    };
    let record =
        ResidentComment::parse_for_write(message).map_err(|error| invalid(error.to_string()))?;
    if record.mandate() != task.id.as_str() {
        return Err(invalid(
            "resident record `mandate` must match the assignment task",
        ));
    }
    ensure_resident_comment_assignment(rt, task)?;
    let comments = rt.get_task_comments(&task.id)?;
    if let Some(cycle) = record.cycle() {
        let live = live_cycle_from_history(&rt.get_task_history(&task.id)?)?;
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
            if comments.iter().any(|comment| {
                matches!(
                    ResidentComment::parse(&comment.message),
                    Ok(ResidentComment::Checkpoint(old))
                        if old.cycle == cycle && old.mandate == task.id.as_str()
                )
            }) {
                return Err(OrbitError::PolicyDenied(
                    "resident_checkpoint_exists: a cycle writes exactly one checkpoint".into(),
                ));
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

#[cfg(feature = "test-support")]
/// Append one checkpoint through the same admission as task comments.
pub fn append_resident_checkpoint(
    rt: &OrbitRuntime,
    task_id: &str,
    record: Value,
    agent: Option<String>,
    model: Option<String>,
) -> Result<Value, OrbitError> {
    append_resident_checkpoint_for_run(rt, task_id, record, agent, model, None)
}

pub(crate) fn append_resident_checkpoint_for_run(
    rt: &OrbitRuntime,
    task_id: &str,
    mut record: Value,
    agent: Option<String>,
    model: Option<String>,
    caller_run_id: Option<&str>,
) -> Result<Value, OrbitError> {
    let initial_message = serde_json::to_string(&record)
        .map_err(|error| invalid(format!("serialize checkpoint: {error}")))?;
    let attempt =
        try_append_resident_checkpoint(rt, task_id, &mut record, &initial_message, agent, model);
    match &attempt {
        Ok(_) => {
            clear_resident_checkpoint_refusal_on_accept(rt, task_id, caller_run_id, Some(&record));
        }
        Err(error) => {
            record_resident_checkpoint_refusal_if_live(
                rt,
                task_id,
                caller_run_id,
                Some(&record),
                error,
            );
        }
    }
    attempt
}

fn try_append_resident_checkpoint(
    rt: &OrbitRuntime,
    task_id: &str,
    record: &mut Value,
    initial_message: &str,
    agent: Option<String>,
    model: Option<String>,
) -> Result<Value, OrbitError> {
    let ResidentComment::Checkpoint(checkpoint) = ResidentComment::parse_for_write(initial_message)
        .map_err(|error| invalid(error.to_string()))?
    else {
        return Err(invalid("checkpoint requires a resident.checkpoint record"));
    };
    if let Ok(Ok(mandate)) = super::resident_mandate_from_artifact(rt, task_id) {
        let history = rt.get_task_history(task_id)?;
        let comments = rt.get_task_comments(task_id)?;
        let prior = super::due::checkpoints(task_id, &comments);
        let forced_continuity = match mandate.continuity {
            orbit_types::workflow::ResidentContinuity::Off => Some("off"),
            orbit_types::workflow::ResidentContinuity::Prefer => {
                let rebuilt_by_fallback =
                    super::due::continuity_rebuilt(&history, &checkpoint.cycle)
                        || rt
                            .execution_env_policy()
                            .process_env()
                            .orbit_activity_resident_continuity()
                            .and_then(OsStr::to_str)
                            == Some("rebuilt");
                // Heartbeat skips schedule another check but do not
                // replace the provider's last real checkpoint.
                let had_resumable_ref = prior
                    .iter()
                    .rev()
                    .find(|checkpoint| checkpoint.outcome != "unchanged")
                    .filter(|last| last.crew == mandate.crew)
                    .and_then(|last| last.conversation_ref.as_deref())
                    .map(str::trim)
                    .is_some_and(|value| !value.is_empty())
                    && super::step::crew_supports_continuity(rt, &mandate.crew);
                if rebuilt_by_fallback || (checkpoint.continuity == "resumed" && !had_resumable_ref)
                {
                    Some("rebuilt")
                } else {
                    None
                }
            }
        };
        if let Some(continuity) = forced_continuity
            && let Some(object) = record.as_object_mut()
        {
            object.insert("continuity".into(), json!(continuity));
        }
    }
    let message = serde_json::to_string(&record)
        .map_err(|error| invalid(format!("serialize checkpoint: {error}")))?;
    if !matches!(
        ResidentComment::parse(&message).map_err(|error| invalid(error.to_string()))?,
        ResidentComment::Checkpoint(_)
    ) {
        return Err(invalid("checkpoint requires a resident.checkpoint record"));
    }
    rt.update_task_with_identity(
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

fn resolve_live_cycle_for_refusal(
    rt: &OrbitRuntime,
    task_id: &str,
    caller_run_id: Option<&str>,
    record: Option<&Value>,
    require_unwritten: bool,
) -> Option<orbit_types::workflow::ResidentLiveCycle> {
    let task = rt.get_task(task_id).ok()?;
    if !task.tags.iter().any(|tag| tag == RESIDENT_TAG) || resident_mandate_ended(task.status) {
        return None;
    }
    let history = rt.get_task_history(task_id).ok()?;
    let live = live_cycle_from_history(&history)
        .ok()
        .flatten()
        .filter(|live| live.expires_at > Utc::now())?;
    if require_unwritten {
        let comments = rt.get_task_comments(task_id).ok()?;
        if checkpoints(task_id, &comments)
            .iter()
            .any(|checkpoint| checkpoint.cycle == live.cycle)
        {
            return None;
        }
    }
    match caller_run_id.map(str::trim).filter(|id| !id.is_empty()) {
        Some(run_id) if run_id == live.run_id => Some(live),
        Some(_) => None,
        None => {
            let record = record?;
            let record_cycle = record.get("cycle").and_then(Value::as_str);
            let record_run = record.get("run_id").and_then(Value::as_str);
            (record_cycle == Some(live.cycle.as_str()) && record_run == Some(live.run_id.as_str()))
                .then_some(live)
        }
    }
}

/// Record a live cycle's checkpoint validation refusal on its run state so
/// terminal synthesis can surface the cause and field without writing to
/// the mandate task on a refused attempt.
pub(crate) fn record_resident_checkpoint_refusal_if_live(
    rt: &OrbitRuntime,
    task_id: &str,
    caller_run_id: Option<&str>,
    record: Option<&Value>,
    error: &OrbitError,
) {
    let OrbitError::InvalidInput(raw) = error else {
        return;
    };
    let Some(live) = resolve_live_cycle_for_refusal(rt, task_id, caller_run_id, record, true)
    else {
        return;
    };
    let Some(refusal) = checkpoint_refusal_from_error_text(&live.cycle, raw) else {
        return;
    };
    let jobs = rt.stores().jobs();
    match jobs.update_run_state(&live.run_id, &mut |_, state| {
        state.resident_checkpoint_refusal = Some(refusal.clone());
        Ok(())
    }) {
        Ok(RunStateUpdate::NoState) => {
            if let Ok(Some(run)) = rt.get_job_run_backend(&live.run_id)
                && run.job_id == RESIDENT_CYCLE_JOB
            {
                let mut state = PipelineState::new(
                    run.run_id,
                    run.job_id,
                    run.input.unwrap_or_else(|| json!({"task_ids": [task_id]})),
                );
                state.resident_checkpoint_refusal = Some(refusal.clone());
                if matches!(jobs.initialize_run_state(&live.run_id, &state), Ok(false)) {
                    let _ = jobs.update_run_state(&live.run_id, &mut |_, existing| {
                        existing.resident_checkpoint_refusal = Some(refusal.clone());
                        Ok(())
                    });
                }
            }
        }
        Ok(_) => {}
        Err(update_error) => {
            tracing::warn!(
                task_id,
                run_id = %live.run_id,
                %update_error,
                "failed to record resident checkpoint validation refusal"
            );
        }
    }
}

pub(crate) fn clear_resident_checkpoint_refusal_on_accept(
    rt: &OrbitRuntime,
    task_id: &str,
    caller_run_id: Option<&str>,
    record: Option<&Value>,
) {
    let Some(live) = resolve_live_cycle_for_refusal(rt, task_id, caller_run_id, record, false)
    else {
        return;
    };
    let _ = rt
        .stores()
        .jobs()
        .update_run_state(&live.run_id, &mut |_, state| {
            if state
                .resident_checkpoint_refusal
                .as_ref()
                .is_some_and(|refusal| refusal.cycle == live.cycle)
            {
                state.resident_checkpoint_refusal = None;
            }
            Ok(())
        });
}

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

fn sanitize_refusal_message(raw: &str) -> String {
    let normalized = raw.replace(['\r', '\n'], " ");
    let redacted = redact_home_dir(&redact_all(&normalized));
    let trimmed = redacted.trim();
    if trimmed.chars().count() <= MAX_CHECKPOINT_REFUSAL_CHARS {
        trimmed.to_string()
    } else {
        let head: String = trimmed.chars().take(MAX_CHECKPOINT_REFUSAL_CHARS).collect();
        format!("{head}…")
    }
}

/// Extract the offending field name from a checkpoint validation message when
/// one is named.
pub(crate) fn extract_checkpoint_refusal_field(message: &str) -> Option<String> {
    let mut rest = message;
    while let Some(start) = rest.find('`') {
        let after_start = &rest[start + 1..];
        let Some(end) = after_start.find('`') else {
            break;
        };
        let token = after_start[..end].trim();
        if !token.is_empty() && token != "resident_record_invalid" {
            return Some(token.to_string());
        }
        rest = &after_start[end + 1..];
    }
    if message.contains("action keys must be unique") {
        return Some("actions".to_string());
    }
    if message.contains("decision ids must be unique") {
        return Some("decisions".to_string());
    }
    if message.contains("checkpoint requires a resident.checkpoint record") {
        return Some("kind".to_string());
    }
    if message.contains("record exceeds 64 KiB") {
        return Some("record".to_string());
    }
    None
}

/// Classify a checkpoint validation or redaction refusal message into a
/// structured [`ResidentCheckpointRefusal`], returning `None` for non-validation
/// refusals such as `resident_cycle_stale` or `resident_checkpoint_exists`.
pub(crate) fn checkpoint_refusal_from_error_text(
    cycle: &str,
    raw_error: &str,
) -> Option<ResidentCheckpointRefusal> {
    let trimmed = raw_error.trim();
    let stripped = trimmed
        .strip_prefix("invalid input: ")
        .or_else(|| trimmed.strip_prefix("policy denied: "))
        .unwrap_or(trimmed);
    if stripped.starts_with("resident_cycle_stale:")
        || stripped.starts_with("resident_checkpoint_exists:")
        || stripped == "the resident mandate has ended"
        || stripped == "resident records belong on a resident mandate's assignment task"
        || stripped.starts_with("the mandate is not usable:")
    {
        return None;
    }
    let code = if stripped.starts_with("resident_record_invalid:") {
        "resident_record_invalid"
    } else if stripped.starts_with("resident records must be supplied without sensitive text") {
        "resident_record_redaction_refused"
    } else {
        "resident_checkpoint_invalid"
    };
    let message = sanitize_refusal_message(stripped);
    let field = extract_checkpoint_refusal_field(&message);
    Some(ResidentCheckpointRefusal {
        cycle: cycle.to_string(),
        code: code.to_string(),
        field,
        message,
    })
}

/// Render a persisted checkpoint refusal into the `error` string carried by a
/// synthesized `incomplete` checkpoint.
pub(crate) fn format_checkpoint_refusal_error(refusal: &ResidentCheckpointRefusal) -> String {
    let prefix = format!("{}:", refusal.code);
    let mut formatted = if refusal.message.starts_with(&prefix) {
        format!("checkpoint validation refused: {}", refusal.message)
    } else {
        format!(
            "checkpoint validation refused ({}): {}",
            refusal.code, refusal.message
        )
    };
    if let Some(field) = refusal.field.as_deref()
        && !formatted.contains(field)
    {
        formatted.push_str(&format!(" (field: {field})"));
    }
    formatted
}

/// Whether a comment string carries a `resident.checkpoint` JSON payload.
pub(crate) fn is_checkpoint_comment_json(message: &str) -> bool {
    serde_json::from_str::<Value>(message)
        .ok()
        .is_some_and(|value| {
            value.get("kind").and_then(Value::as_str) == Some("resident.checkpoint")
        })
}
