use super::{Command, Operation, Outcome};
use crate::application::job::{V2JobRunResult, run_error_step};
use crate::{
    JobRunState, OrbitError, OrbitRuntime, PipelineInvokeResult, PipelineWaitEntry,
    PipelineWaitStatus, find_workflow,
};
use orbit_types::task::TaskStatus;
use serde_json::{Value, json};

const TASK_PILOT_WORKFLOW: &str = "task-pilot";

/// The shipped task-pilot job's `max_tasks`, which prepare also applies when
/// the job's default input declares none.
const SHIPPED_MAX_TASKS: u64 = 50;

/// The selected tasks and pilot settings from the native command.
pub struct PilotRequest {
    pub task_ids: Vec<String>,
    pub base_branch: Option<String>,
    pub max_tasks: Option<u32>,
    pub max_partition_size: Option<u32>,
}

impl PilotRequest {
    /// The total cap on an explicit selection: `--max-tasks` when given, else
    /// the `max_tasks` default of the job the run executes, so a custom
    /// definition's default applies here as it does at preparation.
    fn effective_max_tasks(&self, runtime: &OrbitRuntime) -> Result<u64, OrbitError> {
        if let Some(max_tasks) = self.max_tasks {
            return Ok(u64::from(max_tasks));
        }
        let job_id = find_workflow(TASK_PILOT_WORKFLOW)
            .map(|workflow| workflow.job_id)
            .ok_or_else(|| {
                OrbitError::InvalidInput(format!("unknown workflow '{TASK_PILOT_WORKFLOW}'"))
            })?;
        let default_input = runtime.job_default_input(job_id)?;
        Ok(default_input
            .as_ref()
            .and_then(|input| input.get("max_tasks"))
            .and_then(Value::as_u64)
            .unwrap_or(SHIPPED_MAX_TASKS))
    }
}

impl PilotRequest {
    fn submit(
        self,
        runtime: &OrbitRuntime,
        timeout_seconds: Option<u64>,
    ) -> Result<Vec<WorkflowDispatchResult>, OrbitError> {
        let input = build_task_pilot_input(
            &self.task_ids,
            self.base_branch.as_deref(),
            self.max_tasks,
            self.max_partition_size,
        )?;
        if let Some(branch) = self.base_branch.as_deref() {
            runtime.validate_task_pilot_base_branch(branch)?;
        }
        // Refuse the whole explicit selection before submitting a run. Prepare
        // repeats this check against current state for every other caller and
        // for status changes after this read. Mixed stored crews are submitted
        // as separate, homogeneous runs so no unanimous run fails at startup.
        let mut unsupported = Vec::new();
        let mut by_crew: Vec<(Option<String>, Vec<String>)> = Vec::new();
        for task_id in &self.task_ids {
            let task = runtime.get_task(task_id)?;
            if !matches!(task.status, TaskStatus::Proposed | TaskStatus::Backlog) {
                unsupported.push(format!("{} (status {})", task.id, task.status));
            }
            let crew = task
                .crew
                .as_deref()
                .map(str::trim)
                .filter(|crew| !crew.is_empty())
                .map(ToOwned::to_owned);
            if let Some((_, ids)) = by_crew.iter_mut().find(|(existing, _)| existing == &crew) {
                ids.push(task.id);
            } else {
                by_crew.push((crew, vec![task.id]));
            }
        }
        if !unsupported.is_empty() {
            return Err(OrbitError::InvalidInput(format!(
                "explicit task-pilot selection refuses {}; only proposed and backlog tasks may be assessed",
                unsupported.join(", ")
            )));
        }
        if !self.task_ids.is_empty() {
            let max_tasks = self.effective_max_tasks(runtime)?;
            if self.task_ids.len() as u64 > max_tasks {
                return Err(OrbitError::InvalidInput(format!(
                    "explicit task-pilot selection contains {} tasks, exceeding max_tasks {max_tasks}",
                    self.task_ids.len()
                )));
            }
        }
        let selections = if self.task_ids.is_empty() {
            vec![Vec::new()]
        } else {
            by_crew.into_iter().map(|(_, ids)| ids).collect()
        };
        let mut runs = Vec::new();
        for task_ids in selections {
            let run_input = if task_ids.is_empty() {
                input.clone()
            } else {
                build_task_pilot_input(
                    &task_ids,
                    self.base_branch.as_deref(),
                    self.max_tasks,
                    self.max_partition_size,
                )?
            };
            runs.push(dispatch_pilot(runtime, run_input, timeout_seconds)?);
        }
        Ok(runs)
    }
}

/// Which native spelling submitted the job, for its invocation record.
pub enum JobEntry {
    JobRun,
    RunJob,
}

pub struct SubmittedJob {
    pub invoke: PipelineInvokeResult,
    pub waited: Option<(PipelineWaitEntry, bool)>,
}

impl SubmittedJob {
    fn finish(
        runtime: &OrbitRuntime,
        invoke: PipelineInvokeResult,
        timeout: Option<u64>,
    ) -> Result<Outcome<Self>, OrbitError> {
        let waited = timeout
            .map(|seconds| wait_for_run(runtime, &invoke.run_id, seconds))
            .transpose()?;
        let exit_code = i32::from(
            waited
                .as_ref()
                .is_some_and(|(entry, expired)| *expired || !entry.status.is_success()),
        );
        Ok(Outcome {
            value: Self { invoke, waited },
            exit_code,
        })
    }
}

impl<'a> Command<'a, SubmittedJob> {
    pub fn submit_job(
        entry: JobEntry,
        id: &'a str,
        input: Value,
        timeout: Option<u64>,
        mut warning: impl FnMut(String) + 'a,
    ) -> Self {
        let operation = match entry {
            JobEntry::JobRun => Operation::command("job", "run"),
            JobEntry::RunJob => Operation::command("run", "job"),
        }
        .target(Some("job"), Some(id.to_owned()));
        Self {
            operation,
            flow: Box::new(move |core| {
                let runtime = core.runtime();
                let invoke = runtime.submit_job_run(id, input, None)?;
                for message in [
                    runtime.unset_env_pass_warning(),
                    runtime.unset_crew_env_warning(),
                ]
                .into_iter()
                .flatten()
                {
                    warning(message);
                }
                SubmittedJob::finish(runtime, invoke, timeout)
            }),
        }
    }

    pub fn resume_job(id: &'a str, timeout: Option<u64>) -> Self {
        Self {
            operation: job_run_operation("resume", id),
            flow: Box::new(move |core| {
                let rt = core.runtime();
                SubmittedJob::finish(rt, rt.submit_resume_run(id, None, None)?, timeout)
            }),
        }
    }
}

impl<'a> Command<'a, V2JobRunResult> {
    pub fn replay(id: &'a str, claim_token: Option<&'a str>) -> Self {
        Self {
            operation: job_run_operation("replay", id),
            flow: Box::new(move |core| {
                let value = core.runtime().replay_job_run_with_claim(id, claim_token)?;
                let exit_code = i32::from(!value.success);
                Ok(Outcome { value, exit_code })
            }),
        }
    }
}

impl<'a> Command<'a, ()> {
    pub fn pipeline_worker(id: &'a str) -> Self {
        Self::new(job_run_operation("run-pipeline-worker", id), move |core| {
            core.runtime().execute_pipeline_run_worker(id)
        })
    }
}

impl Command<'static, Vec<WorkflowDispatchResult>> {
    pub fn task_pilot(request: PilotRequest, timeout: Option<u64>) -> Self {
        Self {
            operation: Operation::command("run", "task-pilot")
                .target(Some("workflow"), Some("task-pilot".into())),
            flow: Box::new(move |core| {
                let runs = request.submit(core.runtime(), timeout)?;
                let exit_code = i32::from(runs.iter().any(WorkflowDispatchResult::failed));
                Ok(Outcome {
                    value: runs,
                    exit_code,
                })
            }),
        }
    }
}

#[derive(Clone)]
pub struct WorkflowDispatchResult {
    pub workflow_alias: &'static str,
    pub job_id: String,
    pub run_id: String,
    pub state: String,
    pub wait_timeout: bool,
    pub attempt: u32,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
}

impl WorkflowDispatchResult {
    pub fn failed(&self) -> bool {
        self.wait_timeout
            || matches!(
                self.state.as_str(),
                "failed" | "timeout" | "cancelled" | "interrupted" | "held"
            )
    }
}

/// Validate a CLI wait deadline before submitting anything.
pub fn wait_timeout(raw: Option<u64>) -> Result<u64, OrbitError> {
    OrbitRuntime::normalize_pipeline_wait_timeout(raw)
}

fn job_run_operation(sub: &'static str, id: &str) -> Operation {
    Operation::command("job", sub)
        .target(Some("job_run"), Some(id.into()))
        .job_run(id)
}

/// Keep the core wait-envelope timeout separate from the durable run state.
fn wait_for_run(
    runtime: &OrbitRuntime,
    run_id: &str,
    timeout_seconds: u64,
) -> Result<(PipelineWaitEntry, bool), OrbitError> {
    let wait = runtime.wait_pipeline_runs(
        &[run_id.to_string()],
        timeout_seconds,
        OrbitRuntime::normalize_pipeline_wait_poll_interval(Some(1)),
        None,
    )?;
    let mut entry = wait
        .results
        .into_iter()
        .find(|entry| entry.run_id == run_id)
        .ok_or_else(|| {
            OrbitError::Execution(format!("wait returned no result for run '{run_id}'"))
        })?;
    let run = runtime.show_job_run(run_id)?;
    let wait_timeout =
        entry.status == PipelineWaitStatus::Timeout && run.state != JobRunState::Timeout;
    entry.status = run.state.into();
    Ok((entry, wait_timeout))
}

fn dispatch_pilot(
    runtime: &OrbitRuntime,
    input: Value,
    timeout_seconds: Option<u64>,
) -> Result<WorkflowDispatchResult, OrbitError> {
    let workflow_alias = TASK_PILOT_WORKFLOW;
    let workflow = find_workflow(workflow_alias)
        .ok_or_else(|| OrbitError::InvalidInput(format!("unknown workflow '{workflow_alias}'")))?;
    let invoke = runtime.submit_pipeline_run(workflow.job_id, input, None, None)?;
    let Some(timeout_seconds) = timeout_seconds else {
        let state = if invoke.queued { "queued" } else { "submitted" };
        return Ok(WorkflowDispatchResult {
            workflow_alias,
            job_id: invoke.job_name,
            run_id: invoke.run_id,
            state: state.to_string(),
            wait_timeout: false,
            attempt: 1,
            error_code: None,
            error_message: None,
        });
    };

    let (wait_entry, wait_timeout) = wait_for_run(runtime, &invoke.run_id, timeout_seconds)?;
    let run = runtime.show_job_run(&invoke.run_id)?;
    let run_details = runtime
        .job_history(workflow.job_id)?
        .into_iter()
        .find(|entry| entry.run_id == run.run_id);
    Ok(WorkflowDispatchResult {
        workflow_alias,
        job_id: run.job_id,
        run_id: run.run_id,
        state: wait_entry.status.to_string(),
        wait_timeout,
        attempt: run.attempt,
        error_code: run_details
            .as_ref()
            .and_then(run_error_step)
            .and_then(|step| step.error_code.clone()),
        error_message: wait_entry.error.or_else(|| {
            run_details
                .as_ref()
                .and_then(run_error_step)
                .and_then(|step| step.error_message.clone())
        }),
    })
}

fn build_task_pilot_input(
    task_ids: &[String],
    base_branch: Option<&str>,
    max_tasks: Option<u32>,
    max_partition_size: Option<u32>,
) -> Result<Value, OrbitError> {
    let mut seen = std::collections::HashSet::new();
    for task_id in task_ids {
        if task_id.trim().is_empty() {
            return Err(OrbitError::InvalidInput(
                "task id in explicit task-pilot selection must not be empty".to_string(),
            ));
        }
        if !seen.insert(task_id.as_str()) {
            return Err(OrbitError::InvalidInput(format!(
                "duplicate task id '{task_id}' in explicit task-pilot selection"
            )));
        }
    }

    if task_ids.len() > 500 {
        return Err(OrbitError::InvalidInput(
            "explicit task-pilot selection exceeds the limit of 500 tasks".to_string(),
        ));
    }

    let mut map = serde_json::Map::new();
    if !task_ids.is_empty() {
        map.insert(
            "task_ids".to_string(),
            Value::Array(task_ids.iter().cloned().map(Value::String).collect()),
        );
    }
    if let Some(branch) = base_branch {
        if branch.trim().is_empty() {
            return Err(OrbitError::InvalidInput(
                "task-pilot base branch must not be empty".to_string(),
            ));
        }
        map.insert("base_branch".to_string(), Value::String(branch.to_string()));
    }
    if let Some(max_tasks) = max_tasks {
        map.insert("max_tasks".to_string(), json!(max_tasks));
    }
    if let Some(max_partition_size) = max_partition_size {
        map.insert("max_partition_size".to_string(), json!(max_partition_size));
    }
    Ok(Value::Object(map))
}
