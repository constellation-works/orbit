use clap::Args;
use std::collections::HashSet;

use orbit_core::application::job::agent_invoke_result;
use orbit_core::runtime::audit::run::RunProviderProcess;
use orbit_core::{CatalogReferenceLayer, JobRun, OrbitError, OrbitRuntime};
use orbit_types::workflow::{JobRunState, PipelineState};
use serde_json::{Value, json};

use crate::command::{Block, CommandOut, Execute, Payload};

use super::drain_summary::{pass_throttle_line, pass_waiting_lines, summarize_drain_leaves};
use super::format::{RunRootCause, format_backlog_exclusion_lines, format_root_cause_lines};
use super::job::cli_job_run_to_json_with_activity_provenance;
use super::lock_holders::waiting_lock_holders;
use super::steps::{
    RunDisplaySteps, RunRead, RunStepRecord, StepSource, activity_provenance_lines, filtered_steps,
    legacy_step_to_json, resolve_run, resolve_run_step, run_display_steps, run_header_text,
    run_header_text_with_lock_holders, run_step_record_to_json, step_record_payload,
    step_summary_table,
};

/// `CatalogReferenceLayer::layer` for a definition this binary ships.
const SHIPPED_CATALOG_LAYER: &str = "shipped";

#[derive(Args)]
#[command(
    after_help = "JSON shape: {\"run\":<job-run>,\"pull_claim\":<claim|null>,\"crew_window\":<window|null>,\"catalog_layers\":[{\"reference\":\"job:…|activity:…\",\"layer\":\"workspace|shipped|plugin:<ns>|explicit\",\"shadows\":[…]}],\"pipeline_state\":<state|null>,\"steps\":[<step>],\"steps_source\":\"record|audit\",\"provider_processes\":[{\"pid\":...,\"liveness\":\"alive|exited|unknown\",\"stopped_descendants\":[...],\"blocked_on_stopped_descendant\":<descendant|null>,...}]} or {\"run_id\":...,\"job_id\":...,\"step\":<step>,\"step_output\":<json|null>} with -s.\nThe State: line above is `.run.state`, not a top-level `.state`; `.pipeline_state` is the pipeline checkpoint document and is null for a run that keeps none. `.steps` are the steps this view renders, and `.steps_source` says whether they came from the run record or its audit trail. The `#` column of the table is one-based and Duration is human-readable (`3h 39m 51s`); `.steps[].step_index` stays zero-based and every duration stays in milliseconds. A run whose record holds only a run-level step (an interrupted run's) shows its audit steps instead. The Catalog: lines name only layers other than `shipped` unless `--verbose` is given; `.catalog_layers` always lists every reference. `.pull_claim` (the Claim: line) names the owner task and claim a follower's claimed leaf executes, whether its outcome reached the owner, and its `failure_class` once a failure or release is recorded (`candidate` and `task_input` block the owner's task; `operator_cancel`, `provider`, `environment`, `owner_route`, `baseline_red`, `transient` and `base_conflict` release it to the backlog); it is null for every other run. `.drain_summary` is set for an auto drain only (the Leaves: line): admitted/succeeded/failed/running/cancelled leaf counts, `failed_leaves`, and the backlog its last pass left `waiting`, plus `capacity` (workspace `active_leaf_runs`, `inherited_leaf_runs` outside this coordinator, and `max_active_leaf_runs`, sampled before the last admission wave; the Capacity: line) and `resource_throttle` when host resource pressure held that pass (the Throttled: line, shown for pull drains too from `.pipeline_state.drain_last_pass`); a drain's own `.run.state` says the coordinator ran, not that its leaves shipped. `.pipeline_state.drain_last_pass` also records a pull drain's `last_pass_error`, `consecutive_pass_failures`, and sticky `degraded` warning; after three consecutive failed passes it stops admitting and keeps settling until its window closes. `.claimed_leaves` lists a pull drain's launched leaves that are still running (the Claimed leaves: lines) and is empty for every other run; `.refused_settlements` (the Settlement refused: lines) lists recorded outcomes the owner refused while still holding their claims, each with the owner's `reason`, `refusals`, `retry_after` and `remedy`: the drain requests no new claim while one is held, retries it with backoff (at most every 15 minutes), and `orbit run auto --stop` retries it at once; `.crew_window` (the Crews: lines) is a pull drain's runnable crews and the crews it excluded for its window, each with `source` (`preflight`, `provider_unavailable` or `leaf_released`) and `reason`, and `auth_exclusions` (provider, host, excluded_at, error_class, relogin_hint, credential_source and next_probe_at). Declared auth probes may re-admit these crews in the same window; other exclusions last until the next drain. It is null for every other run; a pull drain being cancelled gracefully stays `running` with `.run.drain_cancel` set (the Cancelling: line) until those leaves finish and settle. An Agent: line marked `blocked=stopped-descendant` (`.provider_processes[].blocked_on_stopped_descendant`) is a live agent waiting on a descendant that has stayed stopped past the supervisor's threshold and is still stopped; `.stopped_descendants` lists every such descendant the supervisor reported, and whether it ended it.\nExamples:\n  orbit run show\n  orbit run show jrun-20260426-0631\n  orbit run show jrun-20260426-0631 -s implement_one --json"
)]
pub struct RunShowArgs {
    /// Run ID to inspect. Defaults to the most recently scheduled run globally.
    pub run_id: Option<String>,

    /// Show a single activity step.id from the v2 job YAML; legacy target ID and index still work
    #[arg(short = 's', long = "step")]
    pub step_id: Option<String>,

    /// Report stored run records as-is: skip stale-run reconciliation, which
    /// finalizes an orphaned pending or running run as interrupted and
    /// releases its task reservations
    #[arg(long)]
    pub no_reconcile: bool,

    /// Also list the catalog layer of every job and activity reference; by
    /// default only references that did not resolve to a shipped definition
    #[arg(long)]
    pub verbose: bool,
}

impl Execute for RunShowArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        run_show_payload(
            runtime,
            self.run_id.as_deref(),
            self.step_id.as_deref(),
            RunRead::from_no_reconcile(self.no_reconcile),
            self.verbose,
        )
    }
}

pub(crate) fn run_show_payload(
    runtime: &OrbitRuntime,
    run_id: Option<&str>,
    step_id: Option<&str>,
    read: RunRead,
    verbose: bool,
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
    // Who holds the locks a waiting run is blocked on, read live: the run
    // only persists the selectors.
    let lock_holders = waiting_lock_holders(runtime, &run, state.as_ref());
    let mut drain_summary = summarize_drain_leaves(&run, state.as_ref(), |child_run_id| {
        read.show(runtime, child_run_id).ok()
    });
    if let Some(summary) = drain_summary.as_mut() {
        for leaf in &mut summary.failed_leaves {
            leaf.resume_run_id = leaf_resume_target(runtime, &leaf.run_id, read);
        }
    }

    let mut run_projection =
        cli_job_run_to_json_with_activity_provenance(runtime, &run, state.as_ref());
    // [ORB-13899] The same audit scan holds the invocation's child: its live
    // progress, and the output reference a failed step never checkpoints.
    run_projection["agent_invocation"] = serde_json::to_value(agent_invoke_result(
        &run,
        state.as_ref().map(|state| &state.step_outputs),
        provider_processes.last(),
    ))
    .unwrap_or(Value::Null);
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
    // A pull drain's live claimed leaves: what a graceful cancel waits for.
    let claimed_leaves = runtime
        .pull_drain_claimed_leaves(&run.run_id)
        .unwrap_or_else(|error| {
            tracing::warn!(target: "orbit.cli.run", run_id = %run.run_id, %error, "pull admissions unreadable; drain shown without its leaves");
            Vec::new()
        });
    // Outcomes a pull drain's owner refused while holding their claims: why,
    // and what the operator does about it [ORB-13979].
    let refused_settlements = runtime
        .pull_drain_refused_settlements(&run.run_id)
        .unwrap_or_else(|error| {
            tracing::warn!(target: "orbit.cli.run", run_id = %run.run_id, %error, "pull admissions unreadable; drain shown without its refused settlements");
            Vec::new()
        });
    // A pull drain's crew window: which crews it runs and which it excluded,
    // and why [ORB-13941]; null for every other run.
    let crew_window = runtime
        .pull_drain_crew_window(&run.run_id)
        .unwrap_or_else(|error| {
            tracing::warn!(target: "orbit.cli.run", run_id = %run.run_id, %error, "pull drain crew window unreadable; drain shown without it");
            None
        });
    let doc = json!({
        "run": run_projection,
        "pull_claim": pull_claim,
        "claimed_leaves": claimed_leaves,
        "refused_settlements": refused_settlements,
        "crew_window": crew_window,
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
        // What a drain's leaves did; null for any run that is not a drain.
        // The drain's own `state` only says the coordinator ran.
        "drain_summary": drain_summary.as_ref().map(|summary| summary.to_json()),
        "waiting_on_lock_holders": lock_holders,
        "root_cause": root_causes.first(),
        "additional_root_causes": root_causes.get(1..).unwrap_or_default(),
        // The same projection the registered/MCP run-show surface emits, so
        // both readers name a live child identically [ORB-11752].
        "provider_processes": provider_processes
            .iter()
            .map(RunProviderProcess::to_json)
            .collect::<Vec<_>>(),
    });

    let mut header = run_header_text_with_lock_holders(&run, state.as_ref(), &lock_holders);
    for id in orbit_core::application::job::job_run_task_ids(&run) {
        let title = match runtime.get_task(&id) {
            Ok(task) => Some(task.title),
            Err(error) => {
                tracing::debug!(%id, %error, "run task title unavailable");
                None
            }
        };
        header.push_str(&format!(
            "\n{} {}{}",
            crate::output::color::bold("Task:"),
            id,
            title.map(|title| format!(" — {title}")).unwrap_or_default()
        ));
    }
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
    header.push_str(&catalog_layer_lines(&catalog_layers, verbose));
    header.push_str(&live_provider_process_lines(&provider_processes));
    header.push_str(&agent_invocation_lines(&doc["run"]["agent_invocation"]));
    header.push_str(&super::security_summary::security_alert_sweep_lines(
        &run,
        state.as_ref(),
    ));
    let exclusion_lines = format_backlog_exclusion_lines(state.as_ref());
    if !exclusion_lines.is_empty() {
        header.push('\n');
        header.push_str(&exclusion_lines.join("\n"));
    }
    if let Some(summary) = &drain_summary {
        header.push('\n');
        header.push_str(&summary.lines(run.state).join("\n"));
    } else if let Some(pass) = state
        .as_ref()
        .and_then(|state| state.drain_last_pass.as_ref())
    {
        // A pull drain records its passes too, without a leaf summary.
        for line in pass_throttle_line(pass)
            .into_iter()
            .chain(pass_waiting_lines(pass))
        {
            header.push('\n');
            header.push_str(&line);
        }
    }
    if let Some(pass) = state
        .as_ref()
        .and_then(|state| state.drain_last_pass.as_ref())
    {
        if let Some(error) = &pass.last_pass_error {
            header.push_str(&format!(
                "\n{} consecutive_failures={} last_pass_error={error}",
                crate::output::color::bold("Pull pass warning:"),
                pass.consecutive_pass_failures,
            ));
        }
        if pass.degraded {
            header.push_str("\nDrain degraded: new admissions stopped; settlement retries continue. Fix the cause, run `orbit run auto --stop`, and start a new drain once this one ends.");
        }
    }
    header.push_str(&claimed_leaf_lines(&run, state.as_ref(), &claimed_leaves));
    for refused in &refused_settlements {
        header.push_str(&format!(
            "\n{} {}",
            crate::output::color::bold("Settlement refused:"),
            refused.describe()
        ));
    }
    if let Some(window) = &crew_window {
        for line in window.describe() {
            header.push_str(&format!(
                "\n{} {line}",
                crate::output::color::bold("Crews:")
            ));
        }
    }
    if steps_source == StepSource::Audit && !steps.is_empty() {
        header.push_str(&format!(
            "\n{} reconstructed from the run audit trail; the run record holds no per-step history",
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

/// The run to resume for a failed leaf: the same choice [`resume_hint`] makes
/// for a run of its own, so the drain's advice and the leaf's agree.
fn leaf_resume_target(runtime: &OrbitRuntime, leaf_run_id: &str, read: RunRead) -> Option<String> {
    let leaf = read.show(runtime, leaf_run_id).ok()?;
    let state = runtime.read_run_state(&leaf.run_id).ok().flatten();
    let causes = collect_failed_leaf_causes(runtime, &leaf, state.as_ref(), read).ok()?;
    if let Some(cause) = causes.iter().find(|cause| {
        cause.run_id != leaf.run_id
            && read
                .show(runtime, &cause.run_id)
                .is_ok_and(|run| is_resumable_state(run.state))
    }) {
        return Some(cause.run_id.clone());
    }
    is_resumable_state(leaf.state).then(|| leaf.run_id.clone())
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

/// A pull drain's graceful cancel and the claimed leaves it still carries.
/// Empty for every other run.
fn claimed_leaf_lines(
    run: &JobRun,
    state: Option<&PipelineState>,
    leaves: &[orbit_core::application::distributed::DrainClaimedLeaf],
) -> String {
    let mut lines = String::new();
    if !run.state.is_terminal()
        && let Some(cancel) = state.and_then(|state| state.drain_cancel.as_ref())
    {
        lines.push_str(&format!(
            "\n{} waiting for {} leaves (requested by {} at {}{}); `orbit run cancel {} --confirm --force` stops them",
            crate::output::color::bold("Cancelling:"),
            leaves.len(),
            cancel.actor,
            cancel.requested_at.format("%Y-%m-%dT%H:%M:%SZ"),
            cancel
                .reason
                .as_deref()
                .map(|reason| format!(", reason={reason}"))
                .unwrap_or_default(),
            run.run_id,
        ));
    }
    if !leaves.is_empty() {
        lines.push_str(&format!(
            "\n{} {} running",
            crate::output::color::bold("Claimed leaves:"),
            leaves.len()
        ));
        for leaf in leaves {
            lines.push_str(&format!("\n  {}", leaf.describe()));
        }
    }
    lines
}

/// One line per catalog reference, naming the layer that resolved it.
///
/// A `shipped` layer is the default answer and says nothing on a 20-activity
/// pipeline, so it is listed only under `--verbose`. Any other layer — a
/// workspace override, a plugin, an unresolved job — is what an operator asking
/// "which file is this step actually running" needs, and always prints.
fn catalog_layer_lines(layers: &[CatalogReferenceLayer], verbose: bool) -> String {
    layers
        .iter()
        .filter(|layer| verbose || layer.layer != SHIPPED_CATALOG_LAYER)
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
/// envelope stopped mid-turn, and the run says `failed`. The answer follows
/// in full, as the agent returned it [ORB-13899]; while the run is open, its
/// newest message and last activity stand in for it. The blob reference
/// names where the complete output is.
pub(super) fn agent_invocation_lines(value: &Value) -> String {
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
    match result.get("answer").and_then(Value::as_object) {
        Some(answer) => lines.push_str(&answer_lines(answer)),
        None => {
            if let Some(summary) = text("summary") {
                lines.push_str(&format!("\n  summary: {summary}"));
            }
        }
    }
    if let Some(progress) = result.get("progress").and_then(Value::as_object) {
        let progress_text = |key: &str| progress.get(key).and_then(Value::as_str);
        if let Some(at) = progress_text("last_activity_at") {
            lines.push_str(&format!("\n  last activity: {at}"));
        }
        // Once the answer is in, the newest sampled message is history.
        if result.get("answer").is_none_or(Value::is_null)
            && let Some(message) = progress_text("latest_message")
        {
            lines.push_str(&labelled_block(
                "latest message",
                message,
                progress
                    .get("latest_message_truncated")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            ));
        }
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

/// The agent's answer: named envelope fields, then every other result field,
/// then its final message.
fn answer_lines(answer: &serde_json::Map<String, Value>) -> String {
    let mut lines = String::new();
    if let Some(summary) = answer.get("summary").and_then(Value::as_str) {
        lines.push_str(&labelled_block("summary", summary, false));
    }
    for key in ["findings", "next_steps"] {
        let items = answer
            .get(key)
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        if items.is_empty() {
            continue;
        }
        lines.push_str(&format!("\n  {key}:"));
        for item in items {
            lines.push_str(&format!(
                "\n    - {}",
                indent(&display_value(item), "      ")
            ));
        }
    }
    for (key, value) in answer
        .get("extra")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        lines.push_str(&labelled_block(key, &display_value(value), false));
    }
    if let Some(message) = answer.get("final_message").and_then(Value::as_str) {
        lines.push_str(&labelled_block(
            "final message",
            message,
            answer
                .get("final_message_truncated")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        ));
    }
    lines
}

/// `label: text` on one line, or the label then the text indented beneath it
/// when the text spans lines.
fn labelled_block(label: &str, text: &str, truncated: bool) -> String {
    let suffix = if truncated {
        "\n    … truncated; full text: orbit run logs <RUN_ID>"
    } else {
        ""
    };
    if text.contains('\n') {
        format!("\n  {label}:\n    {}{suffix}", indent(text, "    "))
    } else {
        format!("\n  {label}: {text}{suffix}")
    }
}

fn indent(text: &str, prefix: &str) -> String {
    text.lines()
        .collect::<Vec<_>>()
        .join(&format!("\n{prefix}"))
}

fn display_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// One line per provider subprocess that has not reported an exit.
///
/// Finished children are omitted: their outcome is already in the step table,
/// and the question this answers is "is the agent still running or is the child
/// lost", which only applies to an open invocation. A live child blocked on a
/// descendant that stayed stopped past the supervisor's threshold is marked
/// `blocked=stopped-descendant`, not shown as plainly alive, and every stopped
/// descendant the supervisor reported follows on its own line.
fn live_provider_process_lines(processes: &[RunProviderProcess]) -> String {
    processes
        .iter()
        .filter(|process| !process.finished)
        .map(|process| {
            let blocked = process.blocked_on_stopped_descendant();
            let mut lines = format!(
                "\n{} provider={} pid={} step={} liveness={}{} started_at={}",
                crate::output::color::bold("Agent:"),
                process.provider.as_deref().unwrap_or("-"),
                process.pid,
                process.step_id.as_deref().unwrap_or("-"),
                process.liveness.as_str(),
                if blocked.is_some() {
                    " blocked=stopped-descendant"
                } else {
                    ""
                },
                process
                    .ts
                    .map(|ts| ts.to_rfc3339())
                    .unwrap_or_else(|| "-".to_string()),
            );
            for descendant in &process.stopped_descendants {
                let outcome = if descendant.ended {
                    "ended by the supervisor".to_string()
                } else {
                    format!(
                        "the supervisor could not end it ({})",
                        descendant.error.as_deref().unwrap_or("no reason recorded")
                    )
                };
                lines.push_str(&format!(
                    "\n  stopped descendant: pid={} command=`{}` stopped for at least {}s; {outcome}{}",
                    descendant.pid,
                    descendant.command.as_deref().unwrap_or("-"),
                    descendant.stopped_ms.unwrap_or(0) / 1000,
                    if descendant.still_stopped {
                        "; still stopped"
                    } else {
                        ""
                    },
                ));
            }
            lines
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
