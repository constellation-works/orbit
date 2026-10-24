//! The full task record JSON every read surface serves.

use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_types::task::{
    Task, TaskStatus, serialize_task_artifacts, task_artifact_manifest_json, task_base_json,
    task_show_record_field_json, task_show_reference_field_json, unknown_task_show_field_message,
};
use serde_json::{Map, Value};

use super::{TaskOsReadout, TaskRow, task_pilot_warnings};
use crate::OrbitRuntime;

impl OrbitRuntime {
    /// `task` with its comment, history and artifact sidecars read from this
    /// runtime's stores.
    pub fn task_row_with_sidecars(&self, task: Task) -> Result<TaskRow, OrbitError> {
        let comments = self.get_task_comments(&task.id)?;
        let history = self.get_task_history(&task.id)?;
        let artifacts = self.get_task_artifact_manifest(&task.id)?;
        Ok(TaskRow {
            task,
            comments,
            history,
            artifacts,
        })
    }
}

/// The full task record: the shared base ([`task_base_json`]), the
/// `comments`, `history` and `artifacts` sidecars, the settled before-PR
/// `review` gate when one exists [ORB-11333], and the host-local crew
/// enrichment ([`OrbitRuntime::task_crew_read`]).
///
/// A single-task show serves [`task_show_json`], which is this record plus
/// `block`, `next_step` and `final_recovery`. Writes and list rows stay on this record.
pub fn task_record_json(
    runtime: &OrbitRuntime,
    row: &TaskRow,
    status_by_id: &BTreeMap<String, TaskStatus>,
) -> Result<Map<String, Value>, OrbitError> {
    let task = &row.task;
    let mut object = task_base_json(task, status_by_id);
    TaskOsReadout::new(runtime)?.insert_into(task, &mut object);
    object.insert(
        "pilot_warnings".into(),
        serde_json::json!(task_pilot_warnings(
            task.status,
            &row.comments,
            &task.dependencies(),
            &|id: &str| task_is_done(runtime, Some(status_by_id), id),
        )),
    );
    object.insert(
        "comments".to_string(),
        serde_json::to_value(&row.comments).map_err(serialize_error("comments"))?,
    );
    object.insert(
        "history".to_string(),
        serde_json::to_value(&row.history).map_err(serialize_error("history"))?,
    );
    object.insert(
        "artifacts".to_string(),
        task_artifact_manifest_json(&row.artifacts),
    );
    if let Some(review) =
        crate::application::review::task_review_projection(runtime, task, &row.artifacts)?
    {
        object.insert("review".to_string(), review);
    }
    runtime.task_crew_read(task).insert_into(&mut object);
    Ok(object)
}

/// The show record: [`task_record_json`] plus the operator guidance only a
/// single-task readout carries.
///
/// `block` explains the latest block or escalation (reason, time and source).
/// `next_step` says what to do about a task a failed run left `blocked`.
/// `final_recovery` is the last automated final-recovery decision, with
/// `run_id`, `decision`, `outcome` and `at`. `owed_criteria` lists each
/// criterion the delivery owes after deploy, with its host, command, target
/// and state. Each key is present only when it applies. `orbit task show`, `orbit.task.show`, `GET /api/tasks/:id` and a
/// routed remote show all serve this record and add only their own extras.
pub fn task_show_json(
    runtime: &OrbitRuntime,
    row: &TaskRow,
    status_by_id: &BTreeMap<String, TaskStatus>,
) -> Result<Map<String, Value>, OrbitError> {
    let mut object = task_record_json(runtime, row, status_by_id)?;
    if let Some(block) = super::blocked_recovery::block_json(row) {
        object.insert("block".to_string(), block);
    }
    if let Some(next) = runtime.blocked_next_step(&row.task, &row.history) {
        object.insert(
            "next_step".to_string(),
            serde_json::to_value(next).map_err(serialize_error("next_step"))?,
        );
    }
    if let Some(record) = super::FinalRecoveryRecord::last(&row.history, &row.comments) {
        object.insert(
            "final_recovery".to_string(),
            serde_json::json!({
                "run_id": record.run_id,
                "decision": record.decision,
                "outcome": record.outcome,
                "at": record.at,
            }),
        );
    }
    if let Some(owed) = super::owed_criteria_json(&row.history) {
        object.insert("owed_criteria".to_string(), owed);
    }
    Ok(object)
}

/// One `orbit.task.show` / `orbit task show --fields` projection.
///
/// A record field answers the value [`task_base_json`] carries for it; a
/// sidecar is read on demand, with `artifacts` in its bounded metadata form;
/// the crew enrichments answer `null` where their case does not apply.
/// `resolved_dependencies` and `relations` need `status_by_id`, which a caller
/// may leave out when it selects neither.
pub fn task_show_field_json(
    runtime: &OrbitRuntime,
    task: &Task,
    field: &str,
    status_by_id: Option<&BTreeMap<String, TaskStatus>>,
) -> Result<Value, OrbitError> {
    match field {
        "pilot_warnings" => Ok(serde_json::json!(task_pilot_warnings(
            task.status,
            &runtime.get_task_comments(&task.id)?,
            &task.dependencies(),
            &|id: &str| task_is_done(runtime, status_by_id, id),
        ))),
        "os_requirement" => Ok(TaskOsReadout::new(runtime)?.requirement(task)),
        "comments" => serde_json::to_value(runtime.get_task_comments(&task.id)?)
            .map_err(serialize_error("comments")),
        "history" => serde_json::to_value(runtime.get_task_history(&task.id)?)
            .map_err(serialize_error("history")),
        "artifacts" => Ok(serialize_task_artifacts(
            &runtime.get_task_artifact_manifest(&task.id)?,
        )),
        "resolved_dependencies" | "relations" => {
            let status_by_id = status_by_id.ok_or_else(|| {
                OrbitError::Execution(format!("missing task status index for {field}"))
            })?;
            Ok(task_show_reference_field_json(task, field, status_by_id).unwrap_or(Value::Null))
        }
        other => task_show_record_field_json(task, other)
            .or_else(|| runtime.task_crew_field_json(task, other))
            .ok_or_else(|| OrbitError::InvalidInput(unknown_task_show_field_message(other))),
    }
}

fn serialize_error(label: &'static str) -> impl FnOnce(serde_json::Error) -> OrbitError {
    move |error| OrbitError::Execution(format!("serialize task {label}: {error}"))
}

fn task_is_done(
    runtime: &OrbitRuntime,
    status_by_id: Option<&BTreeMap<String, TaskStatus>>,
    id: &str,
) -> bool {
    status_by_id
        .and_then(|index| index.get(id).copied())
        .map(|status| status == TaskStatus::Done)
        .unwrap_or_else(|| {
            matches!(
                runtime.get_task(id),
                Ok(found) if found.status == TaskStatus::Done
            )
        })
}
