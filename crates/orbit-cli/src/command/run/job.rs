use orbit_core::application::job::{
    ActivityInvocationEvidence, job_run_to_json_with_activity_provenance,
};
use orbit_core::{
    InvocationQuery, JobRun, OrbitError, OrbitRuntime, PipelineInvokeResult, PipelineWaitEntry,
};
use orbit_types::workflow::PipelineState;
use serde_json::{Value, json};

use clap::Args;

use crate::command::{CommandOut, CommandOutput, Execute, Payload};

/// Terminal wait statuses that mean the submitted run did not succeed.
const FAILED_WAIT_STATUSES: [&str; 4] = ["failed", "timeout", "cancelled", "interrupted"];

#[derive(Args)]
#[command(
    after_help = "Examples:\n  orbit run job task_auto_pipeline\n  orbit run job task_auto_pipeline --input mode=local\n  orbit run job crates/orbit-core/assets/jobs/task_pipeline.yaml --input task_id=T123\n  orbit run job task_pilot_pipeline --wait\n  orbit run job task_pilot_pipeline --input crew=luna --json\n\nThere is no `--crew` flag: crew selection is a run input, `--input crew=<name>`.\nThat picks this run's resolved crew; an activity with an explicit `crew` or\n`system_crew: true` still routes there instead. Check `activity_provenance` in\n`orbit run show <RUN_ID> --json` for what actually dispatched.\n\nThe run is submitted to a detached worker and the command returns as soon as it is durable.\nInspect it with `orbit run history -j <JOB_ID>` and `orbit run show <RUN_ID>`."
)]
pub struct JobRunArgs {
    /// Job ID from the catalog, or a direct path to a schemaVersion 2 job YAML.
    pub job_id: String,
    /// Input key=value pairs passed to all job steps (repeatable).
    /// Example: --input task_id=T123 --input base=main
    #[arg(long)]
    pub input: Vec<String>,
    /// Block until the submitted run reaches a terminal state, and exit
    /// nonzero unless it succeeded.
    #[arg(long)]
    pub wait: bool,
    #[arg(long)]
    pub json: bool,
}

impl Execute for JobRunArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let input = build_job_run_input(&self.input)?;
        // Submission failure — an unknown job, an invalid asset, a worker that
        // could not start — is this command's own failure and surfaces as an
        // error. Everything the submitted run does afterwards is reported by
        // `--wait`, so the two outcomes never share an exit path.
        let invoke = runtime.submit_job_run(&self.job_id, input, None)?;
        if !self.wait {
            return render_submission(&invoke, self.json);
        }

        let timeout_seconds = OrbitRuntime::normalize_pipeline_wait_timeout(None)?;
        let poll_interval_seconds = OrbitRuntime::normalize_pipeline_wait_poll_interval(None);
        let wait = runtime.wait_pipeline_runs(
            std::slice::from_ref(&invoke.run_id),
            timeout_seconds,
            poll_interval_seconds,
            None,
        )?;
        let entry = wait
            .results
            .into_iter()
            .find(|entry| entry.run_id == invoke.run_id)
            .ok_or_else(|| {
                OrbitError::Execution(format!(
                    "wait returned no result for run '{}'",
                    invoke.run_id
                ))
            })?;
        render_wait(&invoke, &entry, self.json)
    }
}

pub(super) fn render_submission(invoke: &PipelineInvokeResult, json_output: bool) -> CommandOut {
    let state = submission_state(invoke);
    if json_output {
        return Ok(Payload::document(json!({
            "job_id": invoke.job_name,
            "run_id": invoke.run_id,
            "state": state,
            "queued": invoke.queued,
            "submitted_at": invoke.submitted_at,
            "waited": false,
        }))
        .into());
    }
    for line in submission_lines(invoke, state) {
        println!("{line}");
    }
    Ok(CommandOutput::Silent)
}

/// Render a completed `--wait`, then fail the command for a non-success
/// terminal state so a caller can branch on the exit status alone.
pub(super) fn render_wait(
    invoke: &PipelineInvokeResult,
    entry: &PipelineWaitEntry,
    json_output: bool,
) -> CommandOut {
    if json_output {
        crate::output::json::print_pretty(&json!({
            "job_id": invoke.job_name,
            "run_id": invoke.run_id,
            "state": entry.status,
            "queued": invoke.queued,
            "submitted_at": invoke.submitted_at,
            "waited": true,
            "finished_at": entry.finished_at,
            "error": entry.error,
            "pipeline": entry.pipeline,
        }))?;
    } else {
        for line in wait_lines(invoke, entry) {
            println!("{line}");
        }
    }

    if FAILED_WAIT_STATUSES.contains(&entry.status.as_str()) {
        let detail = entry
            .error
            .as_deref()
            .map(|error| format!(": {}", single_line(error)))
            .unwrap_or_default();
        return Err(OrbitError::Execution(format!(
            "job run '{}' finished in state '{}'{detail}",
            invoke.run_id, entry.status
        )));
    }
    Ok(CommandOutput::Silent)
}

pub(super) fn submission_state(invoke: &PipelineInvokeResult) -> &'static str {
    if invoke.queued { "queued" } else { "submitted" }
}

pub(super) fn submission_lines(invoke: &PipelineInvokeResult, state: &str) -> Vec<String> {
    vec![
        format!("Job: {}", invoke.job_name),
        format!("Run ID: {}", invoke.run_id),
        format!("State: {state}"),
        inspect_line(invoke),
    ]
}

pub(super) fn wait_lines(invoke: &PipelineInvokeResult, entry: &PipelineWaitEntry) -> Vec<String> {
    let mut lines = vec![
        format!("Job: {}", invoke.job_name),
        format!("Run ID: {}", invoke.run_id),
        format!("State: {}", entry.status),
    ];
    if let Some(finished_at) = &entry.finished_at {
        lines.push(format!("Finished: {finished_at}"));
    }
    if let Some(error) = &entry.error {
        lines.push(format!("Error: {}", single_line(error)));
    }
    lines.push(inspect_line(invoke));
    lines
}

fn inspect_line(invoke: &PipelineInvokeResult) -> String {
    format!(
        "Inspect: orbit run history -j {} | orbit run show {}",
        invoke.job_name, invoke.run_id
    )
}

fn single_line(value: &str) -> String {
    value.replace(['\n', '\r'], " ")
}

#[derive(Args)]
#[command(after_help = "Examples:\n  orbit job replay jrun-task_auto_pipeline-20260505T061300.000")]
pub struct JobReplayArgs {
    /// Source job run ID to replay from step 0.
    pub run_id: String,
    /// Output replay result as JSON.
    #[arg(long)]
    pub json: bool,
}

impl Execute for JobReplayArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let source_run_id = self.run_id;
        let result = runtime.replay_job_run(&source_run_id)?;
        if self.json {
            return Ok(Payload::document(json!({
                "run_id": result.run_id,
                "source_run_id": source_run_id,
                "job_name": result.job_name,
                "success": result.success,
                "message": result.message,
                "pipeline": result.pipeline,
                "events_emitted": result.events_emitted,
            }))
            .into());
        }
        println!(
            "run_id={};replayed_from={};job={};success={};events={}",
            result.run_id, source_run_id, result.job_name, result.success, result.events_emitted,
        );
        if let Some(msg) = &result.message {
            println!("message: {msg}");
        }
        println!(
            "pipeline: {}",
            serde_json::to_string_pretty(&result.pipeline).unwrap_or_default()
        );
        Ok(CommandOutput::Silent)
    }
}

#[derive(Args)]
#[command(
    after_help = "Examples:\n  orbit job resume jrun-20260704-0710\n\nResumes an interrupted (or failed / timed-out) run as a new linked run,\nskipping top-level steps whose checkpoints already recorded success."
)]
pub struct JobResumeArgs {
    /// Source job run ID to resume from its persisted step checkpoints.
    pub run_id: String,
    /// Output resume result as JSON.
    #[arg(long)]
    pub json: bool,
}

impl Execute for JobResumeArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let source_run_id = self.run_id;
        let result = runtime.resume_job_run(&source_run_id)?;
        if self.json {
            return Ok(Payload::document(json!({
                "run_id": result.run_id,
                "resumed_from": source_run_id,
                "job_name": result.job_name,
                "success": result.success,
                "message": result.message,
                "pipeline": result.pipeline,
                "events_emitted": result.events_emitted,
            }))
            .into());
        }
        println!(
            "run_id={};resumed_from={};job={};success={};events={}",
            result.run_id, source_run_id, result.job_name, result.success, result.events_emitted,
        );
        if let Some(msg) = &result.message {
            println!("message: {msg}");
        }
        println!(
            "pipeline: {}",
            serde_json::to_string_pretty(&result.pipeline).unwrap_or_default()
        );
        Ok(CommandOutput::Silent)
    }
}

/// CLI adapter for the shared run projection.
///
/// The CLI has historically exposed the claimed worker PID; the web API does
/// not. Keep that contract difference at this presentation boundary.
pub(crate) fn cli_job_run_to_json(run: &JobRun, state: Option<&PipelineState>) -> Value {
    let evidence = Vec::new();
    let mut value = job_run_to_json_with_activity_provenance(run, state, &evidence);
    value["pid"] = json!(run.pid);
    value
}

/// CLI run inspection enriches the compatible shared projection with durable
/// invocation evidence. Older runs or unavailable telemetry stores remain
/// inspectable; their activity status is explicitly `unavailable` instead of
/// borrowing the requested run crew as an actual model claim.
pub(crate) fn cli_job_run_to_json_with_activity_provenance(
    runtime: &OrbitRuntime,
    run: &JobRun,
    state: Option<&PipelineState>,
) -> Value {
    let evidence = runtime
        .invocation_records(InvocationQuery {
            job_run_id: Some(run.run_id.clone()),
            limit: 1_000,
            ..InvocationQuery::default()
        })
        .unwrap_or_default()
        .into_iter()
        .map(|record| ActivityInvocationEvidence {
            activity_id: record.activity_id,
            provider: record.agent,
            model: record.model,
        })
        .collect::<Vec<_>>();
    let mut value = job_run_to_json_with_activity_provenance(run, state, &evidence);
    value["pid"] = json!(run.pid);
    value
}

#[derive(Args)]
pub struct JobRunPipelineWorkerArgs {
    /// Persisted run ID to claim and execute.
    pub run_id: String,
}

impl Execute for JobRunPipelineWorkerArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        {
            runtime.execute_pipeline_run_worker(&self.run_id)?;
            Ok(CommandOutput::Silent)
        }
    }
}

fn build_job_run_input(pairs: &[String]) -> Result<Value, OrbitError> {
    let mut map = serde_json::Map::new();
    for pair in pairs {
        let (key, value) = pair.split_once('=').ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "invalid --input entry \"{pair}\": expected key=value"
            ))
        })?;
        let key = key.trim();
        if key.is_empty() {
            return Err(OrbitError::InvalidInput(format!(
                "invalid --input entry \"{pair}\": key must not be empty"
            )));
        }
        map.insert(key.to_string(), Value::String(value.to_string()));
    }
    Ok(Value::Object(map))
}
