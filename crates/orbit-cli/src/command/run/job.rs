use std::path::Path;

use orbit_core::application::job::{
    JobRunProjectionEvidence, JobRunProjectionProfile, project_job_run,
};
use orbit_core::{JobRun, OrbitError, OrbitRuntime, PipelineInvokeResult, PipelineWaitEntry};
use orbit_types::workflow::{
    ActivityV2Spec, JobV2, JobV2Step, JobV2StepBody, PipelineState, TASK_PILOT_PIPELINE_JOB,
};
use serde::Deserialize;
use serde_json::{Value, json};

use clap::Args;

use super::support::{WaitArgs, wait_for_run};
use crate::command::{CommandOut, CommandOutput, Execute, Payload};

#[derive(Args)]
#[command(
    after_help = "Examples:\n  orbit run job task_auto_pipeline\n  orbit run job task_auto_pipeline --input mode=local\n  orbit run job task_pilot_pipeline --wait\n  orbit run job task_pilot_pipeline --input crew=luna --json\n\nThere is no `--crew` flag: crew selection is a run input, `--input crew=<name>`.\nThat picks this run's resolved crew; an activity with an explicit `crew` or\n`crew_config_key` pool still routes there instead. Check `activity_provenance` in\n`orbit run show <RUN_ID> --json` for what actually dispatched.\n\nThe run is submitted to a detached worker and the command returns as soon as it is durable.\nInspect it with `orbit run history -j <JOB_ID>` and `orbit run show <RUN_ID>`."
)]
pub struct JobRunArgs {
    /// Job ID from the catalog, or a direct path to a schemaVersion 2 job YAML.
    pub job_id: String,
    /// Input key=value pairs passed to all job steps (repeatable).
    /// Example: --input task_id=T123 --input base=main
    #[arg(long)]
    pub input: Vec<String>,
    #[command(flatten)]
    pub(super) wait: WaitArgs,
}

impl Execute for JobRunArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let input = build_job_run_input(&self.input)?;
        let timeout_seconds = self.wait.timeout_seconds()?;
        reject_unrepresentable_job_run_input(runtime, &self.job_id, &input)?;
        // Submission failure — an unknown job, an invalid asset, a worker that
        // could not start — is this command's own failure and surfaces as an
        // error. Everything the submitted run does afterwards is reported by
        // `--wait`, so the two outcomes never share an exit path.
        let invoke = runtime.submit_job_run(&self.job_id, input, None)?;
        super::support::warn_unset_env_pass(runtime);
        let Some(timeout_seconds) = timeout_seconds else {
            return render_submission(&invoke);
        };

        wait_for_submission(runtime, &invoke, timeout_seconds)
    }
}

fn wait_for_submission(
    runtime: &OrbitRuntime,
    invoke: &PipelineInvokeResult,
    timeout_seconds: u64,
) -> CommandOut {
    let (entry, wait_timeout) = wait_for_run(runtime, &invoke.run_id, timeout_seconds)?;
    render_wait(invoke, &entry, wait_timeout)
}

fn render_submission(invoke: &PipelineInvokeResult) -> CommandOut {
    let state = submission_state(invoke);
    let doc = json!({
        "job_id": invoke.job_name,
        "run_id": invoke.run_id,
        "state": state,
        "queued": invoke.queued,
        "submitted_at": invoke.submitted_at,
        "waited": false,
    });
    Ok(Payload::detail(doc, submission_lines(invoke, state).join("\n")).into())
}

/// Render a completed `--wait`, then fail the command for a non-success
/// terminal state so a caller can branch on the exit status alone.
fn render_wait(
    invoke: &PipelineInvokeResult,
    entry: &PipelineWaitEntry,
    wait_timeout: bool,
) -> CommandOut {
    let doc = json!({
        "job_id": invoke.job_name,
        "run_id": invoke.run_id,
        "state": entry.status,
        "queued": invoke.queued,
        "submitted_at": invoke.submitted_at,
        "waited": true,
        "wait_timeout": wait_timeout,
        "finished_at": entry.finished_at,
        "duration_ms": entry.duration_ms,
        "error": entry.error,
        "pipeline": entry.pipeline,
    });
    let payload = Payload::detail(doc, wait_lines(invoke, entry, wait_timeout).join("\n"));
    if wait_timeout || !entry.status.is_success() {
        return Ok(payload.with_exit_code(1).into());
    }
    Ok(payload.into())
}

fn submission_state(invoke: &PipelineInvokeResult) -> &'static str {
    if invoke.queued { "queued" } else { "submitted" }
}

fn submission_lines(invoke: &PipelineInvokeResult, state: &str) -> Vec<String> {
    vec![
        format!("Job: {}", invoke.job_name),
        format!("Run ID: {}", invoke.run_id),
        format!("State: {state}"),
        inspect_line(invoke),
    ]
}

fn wait_lines(
    invoke: &PipelineInvokeResult,
    entry: &PipelineWaitEntry,
    wait_timeout: bool,
) -> Vec<String> {
    let mut lines = vec![
        format!("Job: {}", invoke.job_name),
        format!("Run ID: {}", invoke.run_id),
        format!("State: {}", entry.status),
    ];
    if wait_timeout {
        lines.push(
            "wait_timeout=true; the wait deadline expired. Inspect this run before resubmitting."
                .to_string(),
        );
    }
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
    /// Workspace claim token held by this caller.
    #[arg(long)]
    pub claim_token: Option<String>,
}

impl Execute for JobReplayArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let source_run_id = self.run_id;
        let result =
            runtime.replay_job_run_with_claim(&source_run_id, self.claim_token.as_deref())?;
        let doc = json!({
            "run_id": result.run_id,
            "source_run_id": source_run_id,
            "job_name": result.job_name,
            "success": result.success,
            "message": result.message,
            "pipeline": result.pipeline,
            "events_emitted": result.events_emitted,
        });
        let mut lines = vec![format!(
            "run_id={};replayed_from={};job={};success={};events={}",
            result.run_id, source_run_id, result.job_name, result.success, result.events_emitted,
        )];
        if let Some(msg) = &result.message {
            lines.push(format!("message: {msg}"));
        }
        lines.push(format!(
            "pipeline: {}",
            serde_json::to_string_pretty(&result.pipeline).unwrap_or_default()
        ));
        let payload = Payload::detail(doc, lines.join("\n"));
        if !result.success {
            return Ok(payload.with_exit_code(1).into());
        }
        Ok(payload.into())
    }
}

#[derive(Args)]
#[command(
    after_help = "Examples:\n  orbit job resume jrun-20260704-0710\n  orbit job resume jrun-20260704-0710 --wait\n\nSubmits a new linked run to a detached worker and returns its run ID.\nCompleted top-level checkpoints are reused. Use `orbit run show <RUN_ID>`\nto inspect it, or `--wait` to block and exit with its terminal outcome."
)]
pub struct JobResumeArgs {
    /// Source job run ID to resume from its persisted step checkpoints.
    pub run_id: String,
    #[command(flatten)]
    pub(super) wait: WaitArgs,
}

impl Execute for JobResumeArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let timeout_seconds = self.wait.timeout_seconds()?;
        let invoke = runtime.submit_resume_run(&self.run_id, None, None)?;
        if let Some(timeout_seconds) = timeout_seconds {
            wait_for_submission(runtime, &invoke, timeout_seconds)
        } else {
            render_submission(&invoke)
        }
    }
}

/// CLI adapter for the shared run projection used by run history.
///
/// History keeps the historical contract of exposing only the claimed worker
/// PID (`ListHistory`); `orbit run show` uses the full-detail profile.
pub(crate) fn cli_job_run_to_json(run: &JobRun, state: Option<&PipelineState>) -> Value {
    project_job_run(
        run,
        JobRunProjectionProfile::ListHistory,
        JobRunProjectionEvidence {
            state,
            activity_evidence: Some(&[]),
            ..JobRunProjectionEvidence::default()
        },
    )
}

/// CLI run inspection enriches the shared full-detail projection with durable
/// invocation evidence. Older runs or unavailable telemetry stores remain
/// inspectable; their activity status is explicitly `unavailable` instead of
/// borrowing the requested run crew as an actual model claim.
pub(crate) fn cli_job_run_to_json_with_activity_provenance(
    runtime: &OrbitRuntime,
    run: &JobRun,
    state: Option<&PipelineState>,
) -> Value {
    let evidence = runtime
        .activity_invocation_evidence(&run.run_id)
        .unwrap_or_default();
    project_job_run(
        run,
        JobRunProjectionProfile::FullDetailStoredSteps,
        JobRunProjectionEvidence {
            state,
            activity_evidence: Some(&evidence),
            ..JobRunProjectionEvidence::default()
        },
    )
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

const PREPARE_TASK_PILOT_ACTION: &str = "prepare_task_pilot";

fn reject_unrepresentable_job_run_input(
    runtime: &OrbitRuntime,
    job_ref: &str,
    input: &Value,
) -> Result<(), OrbitError> {
    let Some(Value::String(raw_task_ids)) = input.get("task_ids") else {
        return Ok(());
    };
    if !is_task_pilot_job(runtime, job_ref) {
        return Ok(());
    }
    Err(OrbitError::InvalidInput(format!(
        "invalid `--input task_ids={raw_task_ids}` for `{job_ref}`: `--input key=value` only \
         passes string values, while task-pilot jobs require `task_ids` to be an array. \
         Use `orbit run task-pilot <TASK_ID>...` for explicit tasks, or omit `--input task_ids=...` \
         (or run `orbit run task-pilot`) for automatic discovery"
    )))
}

fn is_task_pilot_job(runtime: &OrbitRuntime, job_ref: &str) -> bool {
    if job_ref == TASK_PILOT_PIPELINE_JOB {
        return true;
    }
    let direct_path = Path::new(job_ref);
    if direct_path.is_file() {
        return direct_job_uses_task_pilot(direct_path);
    }
    runtime
        .show_job_catalog_entry(job_ref)
        .is_ok_and(|entry| steps_use_action(&entry.spec.steps, PREPARE_TASK_PILOT_ACTION))
}

#[derive(Deserialize)]
struct DirectJobEnvelope {
    metadata: Option<DirectJobMetadata>,
    spec: Option<JobV2>,
}

#[derive(Deserialize)]
struct DirectJobMetadata {
    name: Option<String>,
}

fn direct_job_uses_task_pilot(path: &Path) -> bool {
    let Ok(yaml) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(envelope) = serde_yaml::from_str::<DirectJobEnvelope>(&yaml) else {
        return false;
    };
    envelope
        .metadata
        .and_then(|meta| meta.name)
        .is_some_and(|name| name == TASK_PILOT_PIPELINE_JOB)
        || envelope
            .spec
            .is_some_and(|spec| steps_use_action(&spec.steps, PREPARE_TASK_PILOT_ACTION))
}

fn steps_use_action(steps: &[JobV2Step], action: &str) -> bool {
    steps.iter().any(|step| match &step.body {
        JobV2StepBody::Parallel { parallel } => steps_use_action(&parallel.branches, action),
        JobV2StepBody::FanOut { fan_out, .. } => {
            steps_use_action(std::slice::from_ref(fan_out.worker.as_ref()), action)
        }
        JobV2StepBody::Loop { loop_ } => steps_use_action(&loop_.steps, action),
        JobV2StepBody::TargetRef(reference) => {
            reference.target.strip_prefix("activity:") == Some(action)
        }
        JobV2StepBody::Target(target) => match &target.spec {
            ActivityV2Spec::Deterministic(spec) => spec.action == action,
            ActivityV2Spec::AgentLoop(_) => false,
        },
    })
}
