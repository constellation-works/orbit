//! `auto-task:` job steps (ORB-20158): adapt the engine's requests to the
//! owner adapter in `app::auto_task`. Every failure is a
//! job execution error, so the engine admits nothing after it.

use orbit_common::OrbitError;
use orbit_engine::{AutoTaskAdmission, AutoTaskObservation, AutoTaskStepRequest, DispatchError};
use orbit_types::workflow::{AutoTaskStepRecord, ParkedWait};
use serde_json::Value;

use crate::OrbitRuntime;
use crate::app::auto_task;

pub(super) fn observe_auto_task_step(
    runtime: &OrbitRuntime,
    run_id: &str,
    request: &AutoTaskStepRequest,
) -> Result<AutoTaskObservation, DispatchError> {
    auto_task::observe_step(runtime, run_id, &request.step_id, &request.auto_task)
        .map_err(|error| step_error(run_id, &request.step_id, "observe", error))
}

pub(super) fn checkpoint_auto_task_step(
    runtime: &OrbitRuntime,
    run_id: &str,
    step_id: &str,
    record: &AutoTaskStepRecord,
) -> Result<(), DispatchError> {
    auto_task::checkpoint_step_record(runtime, run_id, step_id, record)
        .map_err(|error| step_error(run_id, step_id, "checkpoint", error))
}

pub(super) fn admit_auto_task_step(
    runtime: &OrbitRuntime,
    run_id: &str,
    step_id: &str,
) -> Result<AutoTaskAdmission, DispatchError> {
    auto_task::admit_step(runtime, run_id, step_id)
        .map_err(|error| step_error(run_id, step_id, "admit", error))
}

pub(super) fn checkpoint_parallel_branch(
    runtime: &OrbitRuntime,
    run_id: &str,
    step_id: &str,
    branch_id: &str,
    output: &Value,
) -> Result<(), DispatchError> {
    auto_task::checkpoint_branch(runtime, run_id, step_id, branch_id, output).map_err(|error| {
        step_error(
            run_id,
            &format!("{step_id}/{branch_id}"),
            "checkpoint",
            error,
        )
    })
}

pub(super) fn park_job_run(
    runtime: &OrbitRuntime,
    run_id: &str,
    park: &ParkedWait,
) -> Result<(), DispatchError> {
    auto_task::park_run(runtime, run_id, park)
        .map_err(|error| DispatchError::JobExecution(format!("park run {run_id}: {error}")))
}

fn step_error(run_id: &str, step_id: &str, action: &str, error: OrbitError) -> DispatchError {
    DispatchError::JobExecution(format!(
        "{action} auto-task step `{step_id}` of run {run_id}: {error}"
    ))
}
