//! The guarded `review -> done` completion transition [ORB-11187].
//!
//! This is the single canonical path a completion-authorized run takes to move
//! delivered work to `done`, shared by local delivery and PR delivery. Keeping
//! it in one place is what makes the authorization invariant testable: the
//! transition refuses any source status other than `review`, so an opt-in
//! `--complete` run can never make a `proposed` task shippable, and it never
//! writes a review verdict, so no independent-review signal is fabricated.
//! A task that still owes a criterion after deploy stays in `review`.
//!
//! `completion.hold` [ORB-23365]: a task whose complexity, type or tags
//! match a rule is held here too, at the canonical transition, so no path
//! through it reaches `done` without a person. Path rules need the diff a
//! route has before its merge — `pr_complete` and local delivery's
//! `git_merge` decide those before anything lands — so this last guard
//! carries no diff of its own.

use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskStatus};
use serde_json::{Value, json};

use crate::context::{RuntimeHost, TaskActivityUpdate};

use super::super::input::{input_string_field, required_job_run_id};
use super::super::vcs::completion_hold::{
    ChangedPaths, evaluate_completion_hold, record_completion_hold,
};

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
    complete_tasks(host, run_id, &task_ids, &authorization_note(input, run_id))
}

/// The transition itself, over an already-resolved task selection.
///
/// PR-mode completion resolves and validates its bundle through the shared
/// handoff context first, so it calls this directly rather than re-deriving the
/// selection from raw input.
pub(in crate::executor::automation) fn complete_tasks<H: RuntimeHost + ?Sized>(
    host: &H,
    run_id: &str,
    task_ids: &[String],
    authorization: &str,
) -> Result<Value, OrbitError> {
    // Validate the whole batch before mutating any task: a refusal for a
    // later task must not leave earlier tasks already completed behind a
    // failed result [ORB-17622].
    let mut skipped_task_ids = Vec::new();
    let mut owed_task_ids = Vec::new();
    let mut planned: Vec<Task> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for task_id in task_ids {
        if !seen.insert(task_id.clone()) {
            continue;
        }
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
        // The latest delivery's unresolved criteria survive candidate reuse
        // by a fresh run, which may skip implementation and create no new
        // owed record. Keep the task in review until they are recorded met.
        if !host.outstanding_owed_criteria(task_id, None)?.is_empty() {
            owed_task_ids.push(task_id.clone());
            continue;
        }
        planned.push(task);
    }

    // [ORB-23365] Hold the whole planned bundle when any planned task's
    // complexity, type or tags match a rule, exactly as a route's own
    // pre-merge decision holds its bundle. `paths` rules cannot match
    // here: the route that merged already read the diff it landed.
    let mut held_task_ids = Vec::new();
    let mut completion_hold = None;
    if !planned.is_empty() {
        let rules = host.completion_hold_rules();
        if let Some(hold) =
            evaluate_completion_hold(&rules, &planned, &ChangedPaths::NoDiff, run_id)?
        {
            held_task_ids = planned.iter().map(|task| task.id.clone()).collect();
            record_completion_hold(host, &held_task_ids, &hold)?;
            completion_hold = Some(hold);
        }
    }

    let mut completed_task_ids = Vec::new();
    for task in &planned {
        if !held_task_ids.is_empty() {
            break;
        }
        host.update_task_from_activity(
            &task.id,
            TaskActivityUpdate {
                status: TaskStatus::Done,
                expected_status: task.status,
                // A completion transition delivers work someone else authored;
                // it must not overwrite that record.
                execution_summary: None,
                comment: None,
                note: Some(authorization.to_string()),
                agent: None,
                model: super::super::vcs::ship_done_attribution(task),
                calling_run_id: Some(run_id.to_string()),
            },
        )?;
        completed_task_ids.push(task.id.clone());
    }

    Ok(json!({
        "phase": "complete",
        "completed_task_ids": completed_task_ids,
        "skipped_task_ids": skipped_task_ids,
        "owed_task_ids": owed_task_ids,
        "held_task_ids": held_task_ids,
        "completion_hold": completion_hold,
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
