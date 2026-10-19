use std::collections::{BTreeMap, BTreeSet};

use orbit_types::task::{ExecutionLocation, TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::{ActivityV2Spec, JobRunState, JobV2, JobV2StepBody, PipelineState};
use serde_json::Value;

use crate::application::task::SYSTEM_ACTOR_LABEL;
use crate::runtime::run_input::carried_task_ids;

/// The run a system block attributes itself to, when resume may undo it.
///
/// Workflow failure and interruption notes carry `, run_id=<id>,`. Final
/// recovery carries `run_id=` inside its escalation prefix. The run's own
/// failure handoff (`pr_failure_handoff`, `pr_conflict_blocked`,
/// `validation_environment_blocked`, `review_gate_escalation` in
/// `executor::automation::vcs::failure`) carries `: run=<id>,`. Any other
/// block, including an operator decision, returns `None` and stays put.
pub(super) fn blocking_run_id(entry: &TaskHistoryEntry) -> Option<&str> {
    if entry.by != SYSTEM_ACTOR_LABEL || entry.to_status != Some(TaskStatus::Blocked) {
        return None;
    }
    let note = entry.note.as_deref()?;
    match entry.event.as_str() {
        orbit_engine::WORKFLOW_RUN_FAILED_EVENT | orbit_engine::WORKFLOW_RUN_INTERRUPTED_EVENT => {
            note.split_once(", run_id=")?.1.split(',').next()
        }
        "final_recovery_escalated" => note
            .strip_prefix("final recovery (run_id=")?
            .split_once(") escalated:")
            .map(|(id, _)| id),
        "pr_failure_handoff"
        | "pr_conflict_blocked"
        | "validation_environment_blocked"
        | "review_gate_escalation"
        | orbit_types::workflow::FORGE_UNAVAILABLE_EXPIRED_EVENT => failure_handoff_run_id(note),
        _ => None,
    }
}

/// `run=<id>` in a failure-handoff note, bounded by the next comma or semicolon.
fn failure_handoff_run_id(note: &str) -> Option<&str> {
    let id = note
        .split_once(": run=")?
        .1
        .split([',', ';'])
        .next()?
        .trim();
    (!id.is_empty()).then_some(id)
}

/// Resolve host actions, never agent claims or a step name alone. A promotion
/// must be in the reused prefix and name the task. Later failed steps and
/// exhaustion of the job do not discard the stage already reached.
pub(super) fn review_checkpoints(job: &JobV2, state: &PipelineState) -> BTreeMap<String, Value> {
    let mut promoted = BTreeMap::new();
    for (index, step) in job.steps.iter().enumerate() {
        let Ok(index) = u32::try_from(index) else {
            break;
        };
        let action = match &step.body {
            JobV2StepBody::Target(target) => match &target.spec {
                ActivityV2Spec::Deterministic(spec) => Some(spec.action.as_str()),
                _ => None,
            },
            _ => None,
        };
        let status = state.step_states.get(&index);
        if action == Some("pr_complete") && status != Some(&JobRunState::Success) {
            return promoted;
        }
        if !matches!(status, Some(JobRunState::Success | JobRunState::Skipped)) {
            break;
        }
        if action != Some("pr_promote") || status != Some(&JobRunState::Success) {
            continue;
        }
        let Some(output) = state.step_outputs.get(&index) else {
            break;
        };
        // A false `when` is checkpointed as success with null output (for
        // example the alternate no-diff promotion in the shipped pipeline).
        if output.is_null() {
            continue;
        }
        if state.pipeline.get(&step.id) != Some(output)
            || output.get("phase").and_then(Value::as_str) != Some("promote")
        {
            break;
        }
        for field in ["performed_task_ids", "reused_task_ids"] {
            for id in output
                .get(field)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                promoted.insert(id.to_string(), output.clone());
            }
        }
    }
    promoted
}

/// The batch/ownership id embedded in the earliest successful checkpoint that
/// carries one. `worktree_setup` is step 0 of every task pipeline and emits
/// both `job_run_id` and `batch_id`, so this resolves to the run that actually
/// owns the reused worktree — which is the id the resumed delivery tail keeps
/// templating into `git_push` / `pr_open` / `pr_promote`.
/// Checkpointed output fields that carry the batch/ownership identity a
/// resumed delivery tail keeps using (`worktree_setup` emits both).
const OWNERSHIP_ID_FIELDS: [&str; 2] = ["job_run_id", "batch_id"];

pub(super) fn checkpoint_ownership_id(state: &PipelineState) -> Option<String> {
    state
        .step_states
        .iter()
        .filter(|(_, step_state)| **step_state == JobRunState::Success)
        .filter_map(|(index, _)| state.step_outputs.get(index))
        .find_map(|output| {
            OWNERSHIP_ID_FIELDS.iter().find_map(|field| {
                output
                    .get(field)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
            })
        })
}

/// Fields that name the tasks a reused delivery checkpoint will act on.
const RESUME_TARGET_ID_FIELDS: &[&str] = &[
    "task_id",
    "task_ids",
    "performed_task_ids",
    "reused_task_ids",
];

/// Tasks this resume must compare with the lineage binding.
///
/// The run input always counts. Checkpoint outputs count only when they carry
/// a batch id, which is the delivery tail that skips `worktree_setup`. A drain
/// checkpoint without that id is left to its own admission.
pub(super) fn resume_binding_task_ids(
    input: &Value,
    resume_state: Option<&PipelineState>,
) -> BTreeSet<String> {
    let mut ids = task_ids_from_input(input).unwrap_or_default();
    let Some(state) = resume_state else {
        return ids;
    };
    if checkpoint_ownership_id(state).is_none() {
        return ids;
    }
    collect_resume_task_ids(&state.initial_input, &mut ids);
    collect_resume_task_ids(&state.pipeline, &mut ids);
    for output in state.step_outputs.values() {
        collect_resume_task_ids(output, &mut ids);
    }
    ids
}

fn collect_resume_task_ids(value: &Value, ids: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if RESUME_TARGET_ID_FIELDS.contains(&key.as_str()) {
                    push_resume_task_ids(child, ids);
                } else {
                    collect_resume_task_ids(child, ids);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_resume_task_ids(item, ids);
            }
        }
        _ => {}
    }
}

fn push_resume_task_ids(value: &Value, ids: &mut BTreeSet<String>) {
    match value {
        Value::String(id) => insert_resume_task_id(id, ids),
        Value::Array(items) => {
            for item in items {
                if let Some(id) = item.as_str() {
                    insert_resume_task_id(id, ids);
                }
            }
        }
        _ => {}
    }
}

fn insert_resume_task_id(id: &str, ids: &mut BTreeSet<String>) {
    let id = id.trim();
    if !id.is_empty() {
        ids.insert(id.to_string());
    }
}

/// Why resume will not adopt `task_id`'s current binding.
pub(super) fn foreign_binding_message(
    task_id: &str,
    source_run_id: &str,
    bound_run: Option<&str>,
    machine: Option<&ExecutionLocation>,
) -> String {
    let binding = match (bound_run, machine) {
        (Some(run_id), Some(location)) => {
            let name = location.machine_name.as_deref().unwrap_or("unnamed");
            format!("job run '{run_id}' on {name} ({})", location.machine_id)
        }
        (Some(run_id), None) => format!("job run '{run_id}'"),
        (None, Some(location)) => {
            let name = location.machine_name.as_deref().unwrap_or("unnamed");
            format!("machine {name} ({})", location.machine_id)
        }
        (None, None) => "no run".to_string(),
    };
    let next = if machine.is_some() {
        "Continue that binding on its machine, or admit a new attempt after an authorized rebind"
    } else {
        "Admit a new attempt after an authorized rebind"
    };
    format!(
        "task '{task_id}' is bound to {binding}, not resume source '{source_run_id}'. \
         Resume refuses before reusing this lineage's checkpoints or rendering its validation input. \
         {next}; resume does not overwrite a foreign claim"
    )
}

/// Task ids the run input explicitly targets, if any. An auto-discovery run
/// names none, in which case lineage ownership is the only scope.
pub(in super::super) fn task_ids_from_input(input: &Value) -> Option<BTreeSet<String>> {
    let ids = carried_task_ids(input);
    (!ids.is_empty()).then_some(ids)
}
