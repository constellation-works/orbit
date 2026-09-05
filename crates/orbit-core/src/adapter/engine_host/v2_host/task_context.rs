use std::path::Path;

use orbit_common::fs::task_io::prune_missing_context_files;
use orbit_engine::{DispatchError, WORKFLOW_RUN_FAILED_EVENT};
use orbit_types::task::{Task, TaskComment, TaskHistoryEntry, TaskStatus};
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::task::{canonicalize_context_files_for_read, context_workspace_root};
use crate::runtime::run_input::singular_task_id_from_input;

/// Ceiling on the number of comments surfaced to an implementing agent.
///
/// Comments are unbounded in principle (an orchestrator can post any number of
/// refinements), but the envelope is a single JSON payload handed to an agent
/// invocation. The newest comments are the ones that can supersede the
/// description (see [ORB-11327]), so truncation must drop the oldest entries
/// first and say so rather than silently dropping the entries that matter.
const MAX_TASK_COMMENTS: usize = 20;

/// Ceiling on the total size, in bytes, of the retained comment bodies.
///
/// Applied after [`MAX_TASK_COMMENTS`] as a second, size-based cut: a handful
/// of very long comments could still blow out the envelope even under the
/// count cap.
const MAX_TASK_COMMENTS_BYTES: usize = 16 * 1024;

pub(crate) fn associated_task_ids(input: &Value) -> Vec<String> {
    let mut task_ids = Vec::new();
    if let Some(task_id) = input.get("task_id").and_then(Value::as_str) {
        push_unique_task_id(&mut task_ids, task_id);
    }
    if let Some(items) = input.get("task_ids").and_then(Value::as_array) {
        for item in items {
            if let Some(task_id) = item.as_str() {
                push_unique_task_id(&mut task_ids, task_id);
            }
        }
    }
    if let Some(items) = input.get("tasks").and_then(Value::as_array) {
        for item in items {
            if let Some(task_id) = item.as_str() {
                push_unique_task_id(&mut task_ids, task_id);
                continue;
            }
            if let Some(task_id) = item
                .get("id")
                .and_then(Value::as_str)
                .or_else(|| item.get("task_id").and_then(Value::as_str))
            {
                push_unique_task_id(&mut task_ids, task_id);
            }
        }
    }
    task_ids
}

pub(crate) fn task_context_for_agent_input(
    runtime: &OrbitRuntime,
    input: &Value,
) -> Result<Option<Value>, DispatchError> {
    let Some(task_id) = singular_task_id_from_input(input) else {
        return Ok(None);
    };
    let task = runtime.get_task(task_id).map_err(|err| {
        DispatchError::CliInvocationFailed(format!(
            "load task `{task_id}` for agent envelope: {err}"
        ))
    })?;
    let task_history = runtime.get_task_history(task_id).map_err(|err| {
        DispatchError::CliInvocationFailed(format!(
            "load task `{task_id}` history for agent envelope: {err}"
        ))
    })?;
    let comments = runtime.get_task_comments(task_id).map_err(|err| {
        DispatchError::CliInvocationFailed(format!(
            "load task `{task_id}` comments for agent envelope: {err}"
        ))
    })?;
    Ok(Some(agent_task_context_json(
        &task,
        &task_history,
        &comments,
        input,
        &runtime.paths().repo_root,
    )))
}

fn agent_task_context_json(
    task: &Task,
    task_history: &[TaskHistoryEntry],
    comments: &[TaskComment],
    input: &Value,
    fallback_repo_root: &Path,
) -> Value {
    let workspace_path = input
        .get("workspace_path")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let repo_root = input
        .get("repo_root")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let prune_root = context_workspace_root(fallback_repo_root, workspace_path.as_deref());
    let canonical_context_files =
        canonicalize_context_files_for_read(&task.context_files, &prune_root);
    let (kept_context_files, _dropped) =
        prune_missing_context_files(&prune_root, canonical_context_files);

    // `json!` with a braced literal always yields `Value::Object`; the fallback
    // arm keeps this total so the agent context never panics on a malformed
    // literal, and takes the map by value instead of cloning it.
    let mut context = match serde_json::json!({
        "id": task.id.clone(),
        "status": task.status.cli_name(),
        "terminal": refuses_implementer_writes(task.status),
        "title": task.title.clone(),
        "description": task.description.clone(),
        "acceptance_criteria": task.acceptance_criteria.clone(),
        "plan": task.plan.clone(),
        "context_files": kept_context_files,
        "tags": task.tags.clone(),
        "required_tools": task.required_tools.clone(),
        "external_refs": task.external_refs.clone(),
        "workspace_path": workspace_path,
        "repo_root": repo_root,
    }) {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };

    if !task.execution_summary.trim().is_empty() {
        context.insert(
            "execution_summary".to_string(),
            Value::String(task.execution_summary.clone()),
        );
    }
    if let Some(status_note) = workflow_failure_status_note(task_history) {
        context.insert(
            "status_note".to_string(),
            Value::String(status_note.to_string()),
        );
    }

    let (kept_comments, omitted_count) = bounded_task_comments(comments);
    context.insert(
        "comments".to_string(),
        serde_json::to_value(kept_comments).unwrap_or_else(|_| Value::Array(Vec::new())),
    );
    if omitted_count > 0 {
        context.insert("comments_truncated".to_string(), Value::Bool(true));
        context.insert(
            "comments_omitted_count".to_string(),
            Value::Number(omitted_count.into()),
        );
    }

    Value::Object(context)
}

/// Keep the newest comments within [`MAX_TASK_COMMENTS`] and
/// [`MAX_TASK_COMMENTS_BYTES`], dropping the oldest entries first so a
/// superseding refinement never falls off the envelope. Returns the retained
/// comments (still in chronological order) and how many oldest entries were
/// dropped.
fn bounded_task_comments(comments: &[TaskComment]) -> (&[TaskComment], usize) {
    let count_start = comments.len().saturating_sub(MAX_TASK_COMMENTS);
    let mut kept = &comments[count_start..];

    while kept.len() > 1 {
        let total_bytes: usize = kept.iter().map(|comment| comment.message.len()).sum();
        if total_bytes <= MAX_TASK_COMMENTS_BYTES {
            break;
        }
        kept = &kept[1..];
    }

    (kept, comments.len() - kept.len())
}

fn workflow_failure_status_note(task_history: &[TaskHistoryEntry]) -> Option<&str> {
    task_history.iter().rev().find_map(|entry| {
        (entry.event == WORKFLOW_RUN_FAILED_EVENT)
            .then_some(entry.note.as_deref())
            .flatten()
            .filter(|note| !note.trim().is_empty())
    })
}

/// Whether the task record refuses the writes an implementer must make.
///
/// Mirrors the `update_task` gate in `command::task::update` and its
/// `orbit.task.update` tool-host twin: `Done` rejects every non-comment
/// mutation, and `Archived` rejects everything except the bare restore to
/// backlog. Neither admits an `execution_summary`, so an implement invocation
/// dispatched against one of these can never persist what it produces.
///
/// An implement invocation is not guaranteed to be the only actor
/// on its task. The executor re-dispatches a failed `agent_implement` step once
/// after its `recovery_activity` succeeds, and a task can be promoted through
/// the review/approve surface while an attempt is still running. Naming the
/// condition in the envelope lets an invocation that has nothing left to do
/// exit up front, instead of discovering it at its final persist call. See
/// The envelope is a dispatch-time snapshot, so `agent_implement` also
/// re-checks status mid-run.
fn refuses_implementer_writes(status: TaskStatus) -> bool {
    matches!(status, TaskStatus::Done | TaskStatus::Archived)
}

fn push_unique_task_id(task_ids: &mut Vec<String>, task_id: &str) {
    let task_id = task_id.trim();
    if !task_id.is_empty() && !task_ids.iter().any(|existing| existing == task_id) {
        task_ids.push(task_id.to_string());
    }
}
