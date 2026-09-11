use clap::Args;
use orbit_core::runtime::run_audit::RunProviderProcess;
use orbit_core::{NotFoundKind, OrbitError, OrbitRuntime};
use serde_json::{Value, json};

use crate::command::{Block, CommandOut, Execute, Payload};

use super::job::cli_job_run_to_json_with_activity_provenance;
use super::steps::{
    RunDisplaySteps, RunStepRecord, StepSource, activity_provenance_lines, filtered_steps,
    legacy_step_to_json, resolve_run, resolve_run_step, run_display_steps, run_header_text,
    run_header_text_with_state, run_step_record_to_json, step_record_payload, step_summary_table,
};

#[derive(Args)]
#[command(
    after_help = "JSON shape: {\"run\":<job-run>,\"pipeline_state\":<state|null>,\"steps\":[<step>],\"steps_source\":\"record|audit\",\"provider_processes\":[{\"pid\":...,\"liveness\":\"alive|exited|unknown\",...}]} or {\"run_id\":...,\"job_id\":...,\"step\":<step>,\"step_output\":<json|null>} with -s.\nThe State: line above is `.run.state`, not a top-level `.state`; `.pipeline_state` is the pipeline checkpoint document and is null for a run that keeps none. `.steps` are the steps this view renders, and `.steps_source` says whether they came from the run record or its audit trail.\nExamples:\n  orbit run show\n  orbit run show jrun-20260426-0631\n  orbit run show jrun-20260426-0631 -s implement_one --json"
)]
pub struct RunShowArgs {
    /// Run ID to inspect. Defaults to the most recently scheduled run globally.
    pub run_id: Option<String>,

    /// Show a single activity step.id from the v2 job YAML; legacy target ID and index still work
    #[arg(short = 's', long = "step")]
    pub step_id: Option<String>,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

impl Execute for RunShowArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        run_show_payload(runtime, self.run_id.as_deref(), self.step_id.as_deref())
    }
}

pub(crate) fn run_show_payload(
    runtime: &OrbitRuntime,
    run_id: Option<&str>,
    step_id: Option<&str>,
) -> CommandOut {
    let run = resolve_run(runtime, run_id)?;
    let state = runtime.read_run_state(&run.run_id)?;

    if let Some(step_id) = step_id {
        let step = resolve_run_step(runtime, &run, step_id)?;
        let step_output = state
            .as_ref()
            .and_then(|state| state.step_outputs.get(&step.step_index))
            .cloned();
        return step_record_payload(&run, &step, step_output);
    }

    // [ORB-10496] Provider subprocesses spawned by this run's agent steps. A
    // ship-pipeline implementation agent is a child of the pipeline worker, not
    // of the Worker daemon, so this is the only place it is observable. The
    // same scan carries the step history a pipeline run keeps nowhere else
    // [ORB-12113].
    let audit = runtime.collect_run_audit_view(&run.run_id)?;
    let provider_processes = audit.provider_processes;

    let RunDisplaySteps {
        records: steps,
        source: steps_source,
    } = run_display_steps(&run, audit.steps);

    let run_projection =
        cli_job_run_to_json_with_activity_provenance(runtime, &run, state.as_ref());
    let doc = json!({
        "run": run_projection,
        "pipeline_state": state,
        // The steps the view below renders, whichever source answered. The
        // record's own `run.steps` stay exactly as stored, so a caller can
        // still tell the two apart [ORB-12113].
        "steps": steps.iter().map(run_step_record_to_json).collect::<Vec<_>>(),
        "steps_source": steps_source.as_str(),
        // The same projection the registered/MCP run-show surface emits, so
        // both readers name a live child identically [ORB-11752].
        "provider_processes": provider_processes
            .iter()
            .map(RunProviderProcess::to_json)
            .collect::<Vec<_>>(),
    });

    let mut header = run_header_text_with_state(&run, state.as_ref());
    if let Some(state) = &state {
        header.push_str(&format!(
            "\n{} iteration={} step_outputs={} updated_at={}",
            crate::output::color::bold("Pipeline:"),
            state.iteration,
            state.step_outputs.len(),
            state.updated_at.to_rfc3339(),
        ));
    }
    header.push_str(&activity_provenance_lines(&doc["run"]["activity_provenance"]).join("\n"));
    if doc["run"]["activity_provenance"]
        .as_array()
        .is_some_and(|values| !values.is_empty())
    {
        header.push('\n');
    }
    header.push_str(&live_provider_process_lines(&provider_processes));
    header.push_str(&agent_invocation_lines(&doc["run"]["agent_invocation"]));
    if steps_source == StepSource::Audit && !steps.is_empty() {
        header.push_str(&format!(
            "\n{} reconstructed from the run audit trail; the run record stores none",
            crate::output::color::bold("Steps:"),
        ));
    }
    header.push('\n');

    Ok(Payload::blocks(
        doc,
        vec![
            Block::text(header),
            Block::table(step_summary_table(&steps)),
        ],
    )
    .into())
}

/// The operator-facing result of an agent invocation run [ORB-11354].
///
/// Empty for every other job. The outcome comes from the run record, not the
/// provider's exit code — an agent that exits 0 without terminating its
/// envelope stopped mid-turn, and the run says `failed`. The preview is
/// bounded; the blob reference names where the rest is.
fn agent_invocation_lines(value: &Value) -> String {
    let Some(result) = value.as_object() else {
        return String::new();
    };
    let text = |key: &str| result.get(key).and_then(Value::as_str);
    let mut lines = format!(
        "\n{} outcome={} envelope_completed={} timed_out={} exit_code={}",
        crate::output::color::bold("Invocation:"),
        text("outcome").unwrap_or("-"),
        result
            .get("completed_envelope")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        result
            .get("timed_out")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        result
            .get("exit_code")
            .and_then(Value::as_i64)
            .map_or_else(|| "-".to_string(), |code| code.to_string()),
    );
    if let Some(reason) = text("failure_reason") {
        lines.push_str(&format!("\n  reason: {reason}"));
    }
    if let Some(summary) = text("summary") {
        lines.push_str(&format!("\n  summary: {summary}"));
    }
    if let Some(blob) = text("stdout_blob_ref") {
        let truncated = result
            .get("preview_truncated")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        lines.push_str(&format!(
            "\n  output: {blob}{} (full text: orbit run logs <RUN_ID>)",
            if truncated {
                " — preview truncated"
            } else {
                ""
            }
        ));
    }
    lines
}

/// One line per provider subprocess that has not reported an exit.
///
/// Finished children are omitted: their outcome is already in the step table,
/// and the question this answers is "is the agent still running or is the child
/// lost", which only applies to an open invocation.
fn live_provider_process_lines(processes: &[RunProviderProcess]) -> String {
    processes
        .iter()
        .filter(|process| !process.finished)
        .map(|process| {
            format!(
                "\n{} provider={} pid={} step={} liveness={} started_at={}",
                crate::output::color::bold("Agent:"),
                process.provider.as_deref().unwrap_or("-"),
                process.pid,
                process.step_id.as_deref().unwrap_or("-"),
                process.liveness.as_str(),
                process
                    .ts
                    .map(|ts| ts.to_rfc3339())
                    .unwrap_or_else(|| "-".to_string()),
            )
        })
        .collect()
}

pub(crate) fn legacy_logs_summary_payload(
    runtime: &OrbitRuntime,
    run_id: &str,
    step_id: Option<&str>,
) -> CommandOut {
    let run = runtime
        .show_job_run(run_id)
        .map_err(|_| OrbitError::not_found(NotFoundKind::JobRun, run_id.to_string()))?;
    let steps = filtered_steps(&run, step_id)?;

    let values = steps
        .iter()
        .map(|step| legacy_step_to_json(step))
        .collect::<Vec<_>>();
    let records = steps
        .iter()
        .map(|step| RunStepRecord::from_job_step(step))
        .collect::<Vec<_>>();

    let mut header = run_header_text(&run);
    header.push('\n');
    Ok(Payload::blocks(
        Value::Array(values),
        vec![
            Block::text(header),
            Block::table(step_summary_table(&records)),
        ],
    )
    .into())
}
