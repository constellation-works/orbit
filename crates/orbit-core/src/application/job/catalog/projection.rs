use std::path::PathBuf;

use orbit_types::workflow::{JobKind, JobRun, JobScheduleState, JobV2, JobV2Step, JobV2StepBody};
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobCatalogFilter {
    WorkflowsOnly,
    All,
    Kind(JobKind),
}

#[derive(Debug, Clone)]
pub struct JobCatalogEntry {
    pub job_id: String,
    pub path: PathBuf,
    pub spec: JobV2,
}

impl JobCatalogEntry {
    /// Whether operator UI surfaces may submit this job without task input.
    /// Delivery coordinators and leaves belong to Ship or a bounded drain.
    pub fn supports_no_input_submission(&self) -> bool {
        let name = &self.job_id;
        self.kind() != JobKind::Subroutine
            && self.state() != JobScheduleState::Disabled
            && !self.spec.holds_task_delivery()
            && !(name.starts_with("workspace_")
                || name.starts_with("epic_")
                || (name.starts_with("task_") && name != "task_pilot_pipeline"))
    }

    pub fn kind(&self) -> JobKind {
        self.spec.kind
    }

    pub fn state(&self) -> JobScheduleState {
        self.spec.state
    }

    pub fn max_active_runs(&self) -> u32 {
        self.spec.max_active_runs
    }

    pub fn default_input(&self) -> Option<&Value> {
        self.spec.default_input.as_ref()
    }
}

pub fn job_catalog_to_json_with_last_run(
    job: &JobCatalogEntry,
    last_run: Option<&JobRun>,
) -> Value {
    let manual_runnable = job.supports_no_input_submission();
    let mut value = json!({
        "job_id": job.job_id.clone(),
        "schema_version": job.spec.schema_version,
        "kind": job.kind().to_string(),
        "state": job.state().to_string(),
        "default_input": job.spec.default_input,
        "max_active_runs": job.spec.max_active_runs,
        "manual_runnable": manual_runnable,
        "supports_no_input_submission": manual_runnable,
        "steps": job.spec.steps.iter().map(job_v2_step_to_json).collect::<Vec<_>>(),
        "path": job.path.display().to_string(),
    });
    if let Some(expire_when) = &job.spec.expire_when {
        value["expire_when"] = json!(expire_when);
    }
    value["last_run_state"] = last_run
        .map(|r| serde_json::Value::String(r.state.to_string()))
        .unwrap_or(serde_json::Value::Null);
    value["last_run_at"] = last_run
        .and_then(|r| r.finished_at.or(r.started_at).or(Some(r.scheduled_at)))
        .map(|ts| serde_json::Value::String(ts.to_rfc3339()))
        .unwrap_or(serde_json::Value::Null);
    value
}

pub(super) fn job_v2_step_to_json(step: &JobV2Step) -> Value {
    let mut value = json!({
        "id": step.id.clone(),
        "when": step.when,
        "retry": step.retry,
    });
    for (key, field) in [
        ("wait_until", &step.wait_until),
        ("expire_when", &step.expire_when),
        ("reuse_when", &step.reuse_when),
        ("require_result", &step.require_result),
    ] {
        if let Some(field) = field {
            value[key] = json!(field);
        }
    }
    match &step.body {
        JobV2StepBody::TargetRef(target) => {
            value["body"] = json!({
                "kind": "target_ref",
                "target": target.target.clone(),
                "default_input": target.default_input,
                "timeout_seconds": target.timeout_seconds,
                "session": target.session,
            });
        }
        JobV2StepBody::Target(target) => {
            value["body"] = json!({
                "kind": "target",
                "default_input": target.default_input,
                "timeout_seconds": target.timeout_seconds,
                "session": target.session,
                "spec": target.spec,
            });
        }
        JobV2StepBody::Parallel { parallel } => {
            value["body"] = json!({
                "kind": "parallel",
                "join": parallel.join,
                "branches": parallel.branches.iter().map(job_v2_step_to_json).collect::<Vec<_>>(),
            });
        }
        JobV2StepBody::FanOut { fan_out, fan_in } => {
            value["body"] = json!({
                "kind": "fan_out",
                "items": fan_out.items,
                "max_workers": fan_out.max_workers,
                "worker": job_v2_step_to_json(&fan_out.worker),
                "fan_in": fan_in,
            });
        }
        JobV2StepBody::Loop { loop_ } => {
            value["body"] = json!({
                "kind": "loop",
                "max_iterations": loop_.max_iterations,
                "break_when": loop_.break_when,
                "steps": loop_.steps.iter().map(job_v2_step_to_json).collect::<Vec<_>>(),
            });
        }
    }
    value
}
