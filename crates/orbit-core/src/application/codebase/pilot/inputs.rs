//! What a task's task-pilot receipts record, and the pilot pipeline steps
//! attribution reads from the job definition.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::workflow::{JobV2Step, JobV2StepBody, TASK_PILOT_PIPELINE_JOB};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::PILOT_ACTOR;
use crate::OrbitRuntime;

/// Apply step ids paired with their pilot worker ids, in pipeline order.
/// Read the job definition so renaming a worker or apply step preserves attribution.
pub(super) fn pilot_steps(runtime: &OrbitRuntime) -> Vec<(String, String)> {
    let job = match runtime.resolved_job_spec(TASK_PILOT_PIPELINE_JOB) {
        Ok(job) => job,
        Err(error) => {
            tracing::warn!(%error, "pilot scorecard: task-pilot definition unreadable");
            return Vec::new();
        }
    };
    job.steps
        .iter()
        .filter_map(|apply| {
            let input = activity_input(apply, "apply_task_pilot_results")?;
            let results = input.get("results")?.as_str()?;
            let worker = job.steps.iter().find_map(|step| {
                let JobV2StepBody::FanOut { fan_out, fan_in } = &step.body else {
                    return None;
                };
                activity_input(&fan_out.worker, "task_pilot")?;
                let collect = fan_in.collect.as_deref().unwrap_or(&step.id);
                (results == format!("{{{{ steps.{collect}.output }}}}"))
                    .then_some(&fan_out.worker.id)
            })?;
            Some((apply.id.clone(), worker.clone()))
        })
        .collect()
}

fn activity_input<'a>(step: &'a JobV2Step, name: &str) -> Option<&'a Value> {
    match &step.body {
        JobV2StepBody::Target(target) if target.activity_name.as_deref() == Some(name) => {
            target.default_input.as_ref()
        }
        JobV2StepBody::TargetRef(target) if target.target == format!("activity:{name}") => {
            target.default_input.as_ref()
        }
        _ => None,
    }
}

fn strings(value: &Value) -> Option<Vec<String>> {
    value.as_array().map(|values| {
        values
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect()
    })
}

/// What a task's task-pilot receipts record, before the newest one is
/// attributed to a run: kept with the task's stored facts, since only its
/// comments change it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(in crate::application::codebase) struct PilotInputs {
    /// The newest assessment's final selectors.
    pub(super) after: Vec<String>,
    /// The filer's selectors before any pilot ran.
    pub(super) author: Vec<String>,
    /// The pilot run the newest assessment names, when it names one.
    pub(super) run_id: Option<String>,
    /// When the newest assessment was written, and its operation id: what a
    /// receipt without a run id is matched by.
    pub(super) at: DateTime<Utc>,
    pub(super) operation_id: String,
}

/// Read a task's task-pilot receipts: `None` when no receipt carries the
/// pilot's final selectors, an error when the comments cannot be read.
pub(in crate::application::codebase) fn inputs(
    runtime: &OrbitRuntime,
    id: &str,
) -> Result<Option<PilotInputs>, OrbitError> {
    let comments = runtime.get_task_pilot_comments(id)?;
    // Comments are stored oldest first.
    let receipts: Vec<_> = comments
        .iter()
        .filter(|comment| comment.by == PILOT_ACTOR)
        .filter_map(|comment| {
            let receipt = comment.receipt()?;
            let body: Value = serde_json::from_str(receipt.body).ok()?;
            Some((comment.at, receipt.operation_id, body))
        })
        .collect();
    let Some((at, operation_id, newest)) = receipts
        .iter()
        .rev()
        .find(|(_, _, body)| body["assessment"]["context_files_after"].is_array())
    else {
        return Ok(None);
    };
    let Some(after) = strings(&newest["assessment"]["context_files_after"]) else {
        return Ok(None);
    };
    let author = receipts
        .first()
        .and_then(|(_, _, oldest)| {
            strings(&oldest["context_files_before"])
                .or_else(|| strings(&oldest["assessment"]["context_files_before"]))
        })
        .unwrap_or_default();
    Ok(Some(PilotInputs {
        after,
        author,
        run_id: newest["pilot_run_id"].as_str().map(str::to_string),
        at: *at,
        operation_id: operation_id.to_string(),
    }))
}
