use clap::Args;
use std::collections::HashSet;

use orbit_core::runtime::audit::run::RunProviderProcess;
use orbit_core::{CatalogReferenceLayer, JobRun, OrbitError, OrbitRuntime};
use orbit_types::workflow::{JobRunState, PipelineState};
use serde_json::{Value, json};

use crate::command::{Block, CommandOut, Execute, Payload};

use super::format::{RunRootCause, format_backlog_exclusion_lines, format_root_cause_lines};
use super::job::cli_job_run_to_json_with_activity_provenance;
use super::steps::{
    RunDisplaySteps, RunRead, RunStepRecord, StepSource, activity_provenance_lines, filtered_steps,
    legacy_step_to_json, resolve_run, resolve_run_step, run_display_steps, run_header_text,
    run_header_text_with_state, run_step_record_to_json, step_record_payload, step_summary_table,
};

#[derive(Args)]
#[command(
    after_help = "JSON shape: {\"run\":<job-run>,\"pull_claim\":<claim|null>,\"catalog_layers\":[{\"reference\":\"job:…|activity:…\",\"layer\":\"workspace|shipped|plugin:<ns>|explicit\",\"shadows\":[…]}],\"pipeline_state\":<state|null>,\"steps\":[<step>],\"steps_source\":\"record|audit\",\"provider_processes\":[{\"pid\":...,\"liveness\":\"alive|exited|unknown\",...}]} or {\"run_id\":...,\"job_id\":...,\"step\":<step>,\"step_output\":<json|null>} with -s.\nThe State: line above is `.run.state`, not a top-level `.state`; `.pipeline_state` is the pipeline checkpoint document and is null for a run that keeps none. `.steps` are the steps this view renders, and `.steps_source` says whether they came from the run record or its audit trail. `.pull_claim` (the Claim: line) names the owner task and claim a follower's claimed leaf executes and whether its outcome reached the owner; it is null for every other run.\nExamples:\n  orbit run show\n  orbit run show jrun-20260426-0631\n  orbit run show jrun-20260426-0631 -s implement_one --json"
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

    /// Report stored run records as-is: skip stale-run reconciliation, which
    /// finalizes an orphaned pending or running run as interrupted and
    /// releases its task reservations
    #[arg(long)]
    pub no_reconcile: bool,
}

impl Execute for RunShowArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        run_show_payload(
            runtime,
            self.run_id.as_deref(),
            self.step_id.as_deref(),
            RunRead::from_no_reconcile(self.no_reconcile),
        )
    }
}

pub(crate) fn run_show_payload(
    runtime: &OrbitRuntime,
    run_id: Option<&str>,
    step_id: Option<&str>,
    read: RunRead,
) -> CommandOut {
    let run = resolve_run(runtime, run_id, read)?;
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
    let root_causes = collect_failed_leaf_causes(runtime, &run, state.as_ref(), read)?;

    let run_projection =
        cli_job_run_to_json_with_activity_provenance(runtime, &run, state.as_ref());
    // Which catalog layer answered for each reference this run's job makes,
    // and what that layer shadowed. A plugin activity a workspace file
    // overrides is otherwise invisible (plugins design §8).
    let catalog_layers = runtime
        .catalog_reference_layers(&run.job_id)
        .unwrap_or_default();
    // A claimed follower leaf names the owner task and claim it works for and
    // whether its outcome reached the owner; every other run has none.
    let pull_claim = runtime.pull_leaf_claim(&run.run_id).unwrap_or_else(|error| {
        tracing::warn!(target: "orbit.cli.run", run_id = %run.run_id, %error, "pull admission unreadable; run shown without its claim");
        None
    });
    let doc = json!({
        "run": run_projection,
        "pull_claim": pull_claim,
        "catalog_layers": catalog_layers
            .iter()
            .map(CatalogReferenceLayer::to_json)
            .collect::<Vec<_>>(),
        "pipeline_state": state,
        // The steps the view below renders, whichever source answered. The
        // record's own `run.steps` stay exactly as stored, so a caller can
        // still tell the two apart [ORB-12113].
        "steps": steps.iter().map(run_step_record_to_json).collect::<Vec<_>>(),
        "steps_source": steps_source.as_str(),
        "root_cause": root_causes.first(),
        "additional_root_causes": root_causes.get(1..).unwrap_or_default(),
        // The same projection the registered/MCP run-show surface emits, so
        // both readers name a live child identically [ORB-11752].
        "provider_processes": provider_processes
            .iter()
            .map(RunProviderProcess::to_json)
            .collect::<Vec<_>>(),
    });

    let mut header = run_header_text_with_state(&run, state.as_ref());
    let cause_lines = format_root_cause_lines(&root_causes);
    if !cause_lines.is_empty() {
        header.push('\n');
        header.push_str(&cause_lines.join("\n"));
    }
    if let Some(hint) = resume_hint(runtime, &run, &root_causes, read) {
        header.push('\n');
        header.push_str(&hint);
    }
    if let Some(state) = &state {
        header.push_str(&format!(
            "\n{} iteration={} step_outputs={} updated_at={}",
            crate::output::color::bold("Pipeline:"),
            state.iteration,
            state.step_outputs.len(),
            state.updated_at.to_rfc3339(),
        ));
    }
    if let Some(claim) = &pull_claim {
        header.push_str(&format!(
            "\n{} {}",
            crate::output::color::bold("Claim:"),
            claim.describe()
        ));
    }
    let provenance_lines = activity_provenance_lines(&doc["run"]["activity_provenance"]);
    if !provenance_lines.is_empty() {
        header.push('\n');
        header.push_str(&provenance_lines.join("\n"));
    }
    header.push_str(&catalog_layer_lines(&catalog_layers));
    header.push_str(&live_provider_process_lines(&provider_processes));
    header.push_str(&agent_invocation_lines(&doc["run"]["agent_invocation"]));
    let exclusion_lines = format_backlog_exclusion_lines(state.as_ref());
    if !exclusion_lines.is_empty() {
        header.push('\n');
        header.push_str(&exclusion_lines.join("\n"));
    }
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

/// Where to resume a failed pipeline run whose failure came from a child run.
///
/// A coordinating run (a ship or drain wrapper) fails only because its child
/// did. Resuming the wrapper re-checks that same failed child result and fails
/// the same way, so the operator needs the run that did the work.
fn resume_hint(
    runtime: &OrbitRuntime,
    run: &JobRun,
    causes: &[RunRootCause],
    read: RunRead,
) -> Option<String> {
    if !is_resumable_state(run.state) {
        return None;
    }
    let leaf = causes.iter().find(|cause| {
        cause.run_id != run.run_id
            && read
                .show(runtime, &cause.run_id)
                .is_ok_and(|leaf| is_resumable_state(leaf.state))
    })?;
    Some(format!(
        "{} the failed work ran in {}; `orbit job resume {}` retries it, while resuming this run only re-checks that child's failure",
        crate::output::color::bold("Resume:"),
        leaf.run_id,
        leaf.run_id,
    ))
}

fn is_resumable_state(state: JobRunState) -> bool {
    matches!(
        state,
        JobRunState::Failed | JobRunState::Timeout | JobRunState::Interrupted
    )
}

/// Follow persisted child dispatches depth first. Only failed terminal leaves
/// are reported; a wrapper's own error stays in its normal run header/steps.
fn collect_failed_leaf_causes(
    runtime: &OrbitRuntime,
    run: &JobRun,
    state: Option<&PipelineState>,
    read: RunRead,
) -> Result<Vec<RunRootCause>, OrbitError> {
    let mut causes = Vec::new();
    let mut visited = HashSet::new();
    collect_failed_leaf_causes_from(runtime, run, state, read, &mut visited, &mut causes)?;
    Ok(causes)
}

fn collect_failed_leaf_causes_from(
    runtime: &OrbitRuntime,
    run: &JobRun,
    state: Option<&PipelineState>,
    read: RunRead,
    visited: &mut HashSet<String>,
    causes: &mut Vec<RunRootCause>,
) -> Result<(), OrbitError> {
    if !visited.insert(run.run_id.clone()) {
        return Ok(());
    }
    let before_children = causes.len();
    for dispatch in state.into_iter().flat_map(|state| &state.child_dispatches) {
        let child = match read.show(runtime, &dispatch.child_run_id) {
            Ok(child) => child,
            Err(OrbitError::NotFound { .. }) => continue,
            Err(error) => return Err(error),
        };
        let child_state = runtime.read_run_state(&child.run_id)?;
        collect_failed_leaf_causes_from(
            runtime,
            &child,
            child_state.as_ref(),
            read,
            visited,
            causes,
        )?;
    }
    if causes.len() != before_children
        || !matches!(
            run.state,
            JobRunState::Failed
                | JobRunState::Timeout
                | JobRunState::Cancelled
                | JobRunState::Interrupted
        )
    {
        return Ok(());
    }

    // V2 runs can store a synthetic job-level step as well as the actual YAML
    // steps in their audit trail. Prefer an audit error so the named step is
    // the one that failed, even when the stored step only wraps that failure.
    let audit_steps = runtime.collect_run_audit_steps(&run.run_id)?;
    let audit_error = audit_steps.iter().rev().find(|step| {
        step.error_message
            .as_deref()
            .is_some_and(|message| !message.is_empty())
    });
    let stored_error = run.steps.iter().rev().find(|step| {
        step.state != JobRunState::Skipped
            && step
                .error_message
                .as_deref()
                .is_some_and(|message| !message.is_empty())
    });
    let audit_failed = audit_steps.iter().rev().find(|step| {
        matches!(
            step.state.as_deref(),
            Some("error" | "failed" | "timeout" | "interrupted")
        )
    });
    let stored_failed = run.steps.iter().rev().find(|step| {
        matches!(
            step.state,
            JobRunState::Failed | JobRunState::Timeout | JobRunState::Interrupted
        )
    });
    let (step, message) = if let Some(step) = audit_error {
        (Some(step.step_id.clone()), step.error_message.clone())
    } else if let Some(step) = stored_error {
        (Some(step.target_id.clone()), step.error_message.clone())
    } else if let Some(step) = audit_failed {
        (Some(step.step_id.clone()), step.error_message.clone())
    } else if let Some(step) = stored_failed {
        (Some(step.target_id.clone()), step.error_message.clone())
    } else {
        (None, None)
    };
    causes.push(RunRootCause {
        run_id: run.run_id.clone(),
        step,
        message,
    });
    Ok(())
}

/// One line per catalog reference, naming the layer that resolved it.
///
/// Printed for every run, not only one that touches a plugin: "which file is
/// this step actually running" is the question, and the answer is the same
/// shape whether a plugin is involved or not.
fn catalog_layer_lines(layers: &[CatalogReferenceLayer]) -> String {
    layers
        .iter()
        .map(|layer| {
            format!(
                "\n{} {}",
                crate::output::color::bold("Catalog:"),
                layer.to_line()
            )
        })
        .collect()
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
        "\n{} outcome={} envelope_completed={} timed_out={} exit_code={} provider_sandbox={}",
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
        text("provider_sandbox").unwrap_or("-"),
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
    let run = runtime.show_job_run(run_id)?;
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
