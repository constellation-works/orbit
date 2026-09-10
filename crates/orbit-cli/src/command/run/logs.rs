use clap::Args;
use orbit_core::application::job::PipelineWorkerLogSnapshot;
use orbit_core::runtime::run_audit::RunCliInvocationRecord;
use orbit_core::{JobRun, OrbitRuntime};
use serde_json::{Value, json};

use crate::command::{CommandOut, Execute, Payload};

use super::steps::{resolve_run, resolve_step_filter};

#[derive(Args)]
#[command(
    after_help = "JSON shape: {\"run_id\":\"...\",\"job_id\":\"...\",\"records\":[{\"step_id\":...,\"stdout_blob_ref\":...,\"stderr_blob_ref\":...,\"stdout\":\"...\",\"stderr\":\"...\"}]}\nExamples:\n  orbit run logs\n  orbit run logs jrun-20260426-0631\n  orbit run logs jrun-20260426-0631 -s implement_one --json"
)]
pub struct RunLogsArgs {
    /// Run ID to inspect. Defaults to the most recently scheduled run globally.
    pub run_id: Option<String>,

    /// Show raw logs for a single activity step.id from the v2 job YAML
    #[arg(short = 's', long = "step")]
    pub step_id: Option<String>,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

impl Execute for RunLogsArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        run_logs_payload(runtime, self.run_id.as_deref(), self.step_id.as_deref())
    }
}

pub(crate) fn run_logs_payload(
    runtime: &OrbitRuntime,
    run_id: Option<&str>,
    step_id: Option<&str>,
) -> CommandOut {
    let run = resolve_run(runtime, run_id)?;
    let audit_steps = runtime.collect_run_audit_steps(&run.run_id)?;
    let step_filter = resolve_step_filter(&run, &audit_steps, step_id)?;
    let records = filter_cli_invocation_records(
        runtime.collect_run_cli_invocations(&run.run_id)?,
        step_filter.as_deref(),
    );

    if records.is_empty() {
        return worker_log_fallback_payload(runtime, &run);
    }

    let doc = json!({
        "run_id": run.run_id,
        "job_id": run.job_id,
        "records": records.iter().map(cli_invocation_record_to_json).collect::<Vec<_>>(),
    });

    // Subprocess stderr is diagnostic: keep it off the record stream so
    // `--format json` stdout stays parseable.
    for record in &records {
        eprint!("{}", record.stderr);
    }
    let text = records
        .iter()
        .map(|record| record.stdout.as_str())
        .collect::<String>();
    Ok(Payload::detail(doc, text).into())
}

/// [ORB-12038] `records` is empty for a run that never reached step
/// execution — most notably a routine-dispatch workspace mismatch, which
/// fails the run before any step can run and so has no audited CLI
/// invocation to show. That worker still writes its own
/// `<run_id>.worker.log` directly; fall back to it instead of reporting no
/// logs when the file is actually sitting on disk.
fn worker_log_fallback_payload(runtime: &OrbitRuntime, run: &JobRun) -> CommandOut {
    let worker_log = runtime.read_pipeline_worker_log(&run.run_id)?;
    let doc = json!({
        "run_id": run.run_id,
        "job_id": run.job_id,
        "records": Value::Array(Vec::new()),
        "worker_log_path": worker_log.as_ref().map(|snapshot| snapshot.path.display().to_string()),
    });
    let detail = match worker_log {
        Some(PipelineWorkerLogSnapshot {
            path,
            content: Some(content),
        }) => format!("worker log ({}):\n{content}", path.display()),
        Some(PipelineWorkerLogSnapshot {
            path,
            content: None,
        }) => format!(
            "worker log recorded with no readable content: {}",
            path.display()
        ),
        None => "No raw stdout/stderr blobs recorded.".to_string(),
    };
    Ok(Payload::detail(doc, detail).into())
}

fn filter_cli_invocation_records(
    records: Vec<RunCliInvocationRecord>,
    step_filter: Option<&str>,
) -> Vec<RunCliInvocationRecord> {
    records
        .into_iter()
        .filter(|record| step_filter.is_none_or(|filter| record.step_id.as_deref() == Some(filter)))
        .collect()
}

fn cli_invocation_record_to_json(record: &RunCliInvocationRecord) -> Value {
    json!({
        "step_id": record.step_id,
        "provider": record.provider,
        "stdout_blob_ref": record.stdout_blob_ref,
        "stderr_blob_ref": record.stderr_blob_ref,
        "stdout": record.stdout,
        "stderr": record.stderr,
    })
}
