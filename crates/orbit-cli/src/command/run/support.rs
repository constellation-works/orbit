use std::fmt::Write as _;

use orbit_core::{JobRun, JobRunState, JobRunStep, OrbitError, OrbitRuntime, find_workflow};
use serde_json::{Value, json};

use crate::command::{CommandOut, Payload};

pub(super) const TASK_AUTO_PIPELINE_JOB: &str = "task_auto_pipeline";

#[derive(Clone)]
pub(crate) struct WorkflowDispatchResult {
    pub workflow_alias: &'static str,
    pub job_id: String,
    pub run_id: String,
    pub state: String,
    pub attempt: u32,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
}

pub(crate) fn dispatch_workflow(
    runtime: &OrbitRuntime,
    workflow_alias: &'static str,
    input: &Value,
    debug: bool,
    wait_for_completion: bool,
    loop_count: u32,
) -> Result<Vec<WorkflowDispatchResult>, OrbitError> {
    let workflow = find_workflow(workflow_alias)
        .ok_or_else(|| OrbitError::InvalidInput(format!("unknown workflow '{workflow_alias}'")))?;
    if debug {
        return Err(OrbitError::InvalidInput(
            "`orbit run --debug` is not supported for persisted workflow runs; use `orbit job run <path>` for direct schemaVersion 2 job debugging.".to_string(),
        ));
    }

    let mut results = Vec::with_capacity(loop_count as usize);
    for _ in 0..loop_count {
        let invoke = runtime.submit_pipeline_run(workflow.job_id, input.clone(), None, None)?;
        if !wait_for_completion {
            let state = if invoke.queued { "queued" } else { "submitted" };
            results.push(WorkflowDispatchResult {
                workflow_alias,
                job_id: invoke.job_name,
                run_id: invoke.run_id,
                state: state.to_string(),
                attempt: 1,
                error_code: None,
                error_message: None,
            });
            continue;
        }

        let timeout_seconds = OrbitRuntime::normalize_pipeline_wait_timeout(None)?;
        let poll_interval_seconds = OrbitRuntime::normalize_pipeline_wait_poll_interval(None);
        let wait = runtime.wait_pipeline_runs(
            std::slice::from_ref(&invoke.run_id),
            timeout_seconds,
            poll_interval_seconds,
            None,
        )?;
        let run = runtime.show_job_run(&invoke.run_id)?;
        let run_details = runtime
            .job_history(workflow.job_id)?
            .into_iter()
            .find(|entry| entry.run_id == run.run_id);
        let wait_entry = wait
            .results
            .into_iter()
            .find(|entry| entry.run_id == run.run_id);
        results.push(WorkflowDispatchResult {
            workflow_alias,
            job_id: run.job_id,
            run_id: run.run_id,
            state: wait_entry
                .as_ref()
                .map(|entry| entry.status.clone())
                .unwrap_or_else(|| run.state.to_string()),
            attempt: run.attempt,
            error_code: run_details
                .as_ref()
                .and_then(summary_step)
                .and_then(|step| step.error_code.clone()),
            error_message: wait_entry.and_then(|entry| entry.error).or_else(|| {
                run_details
                    .as_ref()
                    .and_then(summary_step)
                    .and_then(|step| step.error_message.clone())
            }),
        });
    }

    Ok(results)
}

pub(crate) fn workflow_dispatch_payload(
    workflow_alias: &'static str,
    runs: &[WorkflowDispatchResult],
) -> CommandOut {
    let doc = if runs.len() == 1 {
        workflow_dispatch_result_to_json(&runs[0])
    } else {
        json!({
            "workflow": workflow_alias,
            "runs": runs
                .iter()
                .map(workflow_dispatch_result_to_json)
                .collect::<Vec<_>>(),
        })
    };
    let text = runs
        .iter()
        .flat_map(workflow_dispatch_result_lines)
        .collect::<Vec<_>>()
        .join("\n");
    Ok(Payload::detail(doc, text).into())
}

fn summary_step(run: &JobRun) -> Option<&JobRunStep> {
    run.steps
        .iter()
        .rev()
        .find(|step| step.error_code.is_some() || step.error_message.is_some())
        .or_else(|| {
            run.steps.iter().rev().find(|step| {
                matches!(
                    step.state,
                    JobRunState::Failed
                        | JobRunState::Timeout
                        | JobRunState::Cancelled
                        | JobRunState::Interrupted
                )
            })
        })
        .or_else(|| {
            run.steps
                .iter()
                .rev()
                .find(|step| step.state != JobRunState::Skipped)
        })
        .or_else(|| run.steps.last())
}

pub(super) fn workflow_dispatch_result_to_json(run: &WorkflowDispatchResult) -> Value {
    let value = json!({
        "workflow": run.workflow_alias,
        "job_id": run.job_id,
        "run_id": run.run_id,
        "state": run.state,
        "attempt": run.attempt,
        "error_code": run.error_code,
        "error_message": run.error_message,
    });
    value
}

pub(super) fn workflow_dispatch_result_lines(run: &WorkflowDispatchResult) -> Vec<String> {
    if matches!(run.state.as_str(), "submitted" | "queued")
        && run.error_code.is_none()
        && run.error_message.is_none()
    {
        return vec![
            format!("Workflow: {}", run.workflow_alias),
            format!("Job ID: {}", run.job_id),
            format!("Run ID: {}", run.run_id),
            format!("State: {}", run.state),
            format!(
                "Inspect: orbit run history -j {} | orbit run show {}",
                run.job_id, run.run_id
            ),
        ];
    }

    let error_code = run.error_code.clone().unwrap_or_else(|| "-".to_string());
    let error_message = single_line(run.error_message.as_deref().unwrap_or("-"));
    let mut first = format!(
        "workflow={};job_id={};run_id={};state={};attempt={}",
        run.workflow_alias, run.job_id, run.run_id, run.state, run.attempt
    );
    let _ = write!(
        first,
        ";error_code={};error_message={}",
        single_line(&error_code),
        error_message
    );

    vec![first]
}

fn single_line(value: &str) -> String {
    value.replace(['\n', '\r'], " ")
}
