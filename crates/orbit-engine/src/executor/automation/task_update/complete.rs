//! The guarded `review -> done` completion transition [ORB-11187].
//!
//! This is the single canonical path a completion-authorized run takes to move
//! delivered work to `done`, shared by local delivery and PR delivery. Keeping
//! it in one place is what makes the authorization invariant testable: the
//! transition refuses any source status other than `review`, so an opt-in
//! `--complete` run can never make a `proposed` task shippable, and it never
//! writes a review verdict, so no independent-review signal is fabricated.

use orbit_common::OrbitError;
use orbit_types::task::TaskStatus;
use serde_json::{Value, json};

use crate::context::{RuntimeHost, TaskActivityUpdate};

use super::super::input::{input_string_field, required_job_run_id};

/// Apply the guarded completion transition to every task named by the input.
///
/// `task_ids` (or the single `task_id`) selects the tasks. Already-`done` tasks
/// are skipped so a resumed run is idempotent; any task that is neither `review`
/// nor `done` is an error, because reaching completion from there would skip the
/// delivery the authorization was granted for.
pub(in crate::executor::automation) fn task_complete<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
) -> Result<Value, OrbitError> {
    let run_id = required_job_run_id(input, "task_complete")?;
    let task_ids = completion_task_ids(input)?;
    complete_tasks(host, &task_ids, &authorization_note(input, run_id))
}

/// The transition itself, over an already-resolved task selection.
///
/// PR-mode completion resolves and validates its bundle through the shared
/// handoff context first, so it calls this directly rather than re-deriving the
/// selection from raw input.
pub(in crate::executor::automation) fn complete_tasks<H: RuntimeHost + ?Sized>(
    host: &H,
    task_ids: &[String],
    authorization: &str,
) -> Result<Value, OrbitError> {
    let mut completed_task_ids = Vec::new();
    let mut skipped_task_ids = Vec::new();
    for task_id in task_ids {
        let task = host.get_task(task_id)?;
        match task.status {
            TaskStatus::Done => {
                skipped_task_ids.push(task_id.clone());
                continue;
            }
            TaskStatus::Review => {}
            other => {
                return Err(OrbitError::Execution(format!(
                    "task_complete: task '{task_id}' must be in review to be completed; current status is {other}"
                )));
            }
        }

        host.update_task_from_activity(
            task_id,
            TaskActivityUpdate {
                status: TaskStatus::Done,
                // A completion transition delivers work someone else authored;
                // it must not overwrite that record.
                execution_summary: None,
                comment: None,
                note: Some(authorization.to_string()),
                agent: None,
                model: super::super::vcs::ship_done_attribution(&task),
            },
        )?;
        completed_task_ids.push(task_id.clone());
    }

    Ok(json!({
        "phase": "complete",
        "completed_task_ids": completed_task_ids,
        "skipped_task_ids": skipped_task_ids,
        "authorization": authorization,
    }))
}

/// The durable authorization provenance recorded on each transition.
///
/// The run id is always present and links the transition back to the run whose
/// persisted input carries `completion: done`; `authorized_by` names the
/// submitting operator when the submission surface supplied one.
pub(in crate::executor::automation) fn authorization_note(input: &Value, run_id: &str) -> String {
    match input_string_field(input, "authorized_by") {
        Some(actor) => {
            format!("completion authorized by operator '{actor}' for run {run_id}")
        }
        None => format!("completion authorized by run {run_id}"),
    }
}

fn completion_task_ids(input: &Value) -> Result<Vec<String>, OrbitError> {
    let mut task_ids = Vec::new();
    for key in ["task_ids", "completed_task_ids"] {
        if let Some(values) = input.get(key).and_then(Value::as_array) {
            task_ids.extend(
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned),
            );
        }
    }
    if let Some(task_id) = input_string_field(input, "task_id") {
        task_ids.push(task_id);
    }
    let mut seen = std::collections::HashSet::new();
    task_ids.retain(|task_id| seen.insert(task_id.clone()));
    if task_ids.is_empty() {
        return Err(OrbitError::InvalidInput(
            "task_complete requires a non-empty input.task_ids or input.task_id".to_string(),
        ));
    }
    Ok(task_ids)
}
