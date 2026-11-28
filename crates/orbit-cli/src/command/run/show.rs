use clap::Args;
use orbit_core::app::run::{Command, exec};
use std::collections::HashSet;

use orbit_common::OrbitError;
use orbit_core::Core;
use orbit_core::app::run::CatalogReferenceLayer;
use orbit_core::app::run::{ClaimedLeafStage, DrainClaimedLeaf};
use orbit_core::app::run::{
    RunFailureCause, agent_invoke_result, display_steps_with_agent_execution_to_json,
};
use orbit_core::app::run::{RunProviderProcess, RunScriptExecution};
use orbit_types::workflow::JobRun;
use orbit_types::workflow::PipelineState;
use serde_json::{Value, json};

use crate::command::{Block, CommandOut, ExecuteCore, Payload};

use super::drain_summary::{leaf_lines, pass_refusal_line, pass_throttle_line, pass_waiting_lines};
use super::format::{
    WaitingTaskFormatter, agent_invocation_lines, catalog_layer_lines,
    format_backlog_exclusion_lines, format_root_cause_lines, live_provider_process_lines,
};
use super::job::cli_job_run_to_json_with_activity_provenance;
use super::steps;
use super::steps::{
    RunRead, StepSource, run_header_text_with_lock_holders, step_record_payload, step_summary_table,
};
use orbit_core::app::run::{RunDetail, ShownRun};

#[derive(Args)]
#[command(
    after_help = "JSON shape, step index base and duration units: installed orbit-orchestrate skill `references/run-debugging-audit-output.md`.\n\nExamples:\n  orbit run show\n  orbit run show jrun-20260426-0631\n  orbit run show jrun-20260426-0631 -s implement_one --json"
)]
pub struct RunShowArgs {
    /// Run ID to inspect. Defaults to the most recently scheduled run globally.
    pub run_id: Option<String>,

    /// Show a single activity step.id from the v2 job YAML
    #[arg(short = 's', long = "step")]
    pub step_id: Option<String>,

    /// Report stored run records as-is: skip stale-run reconciliation, which
    /// finalizes an orphaned pending or running run as interrupted, releases
    /// its task reservations, and settles with their owner the pull claims an
    /// ended drain or leaf left
    #[arg(long)]
    pub no_reconcile: bool,

    /// Also list the catalog layer of every job and activity reference; by
    /// default only references that did not resolve to a shipped definition
    #[arg(long)]
    pub verbose: bool,
}

impl ExecuteCore for RunShowArgs {
    fn execute(self, core: &Core) -> CommandOut {
        run_show_payload(
            core,
            self.run_id.as_deref(),
            self.step_id.as_deref(),
            RunRead::from_no_reconcile(self.no_reconcile),
            self.verbose,
        )
    }
}

pub(crate) fn run_show_payload(
    core: &Core,
    run_id: Option<&str>,
    step_id: Option<&str>,
    read: RunRead,
    verbose: bool,
) -> CommandOut {
    let detail = match exec(
        core,
        Command::show(run_id, step_id, read, |message| eprintln!("{message}")),
    )?
    .value
    {
        ShownRun::Step(step) => {
            let (run, step, output) = *step;
            return step_record_payload(&run, &step, output);
        }
        ShownRun::Full(detail) => detail,
    };
    let RunDetail {
        run,
        state,
        checkpoint_unreadable,
        provider_processes,
        script_executions,
        recovery,
        tag_instructions,
        display_steps,
        root_causes,
        lock_holders,
        drain_summary,
        catalog_layers,
        pull,
        effort_evidence,
        auto_concurrency,
        task_titles,
        resumable_leaf,
        agent_activity_ids,
        waiting_tasks,
        worker_limit,
        url,
    } = *detail;
    let steps_source = display_steps.source;
    let rescue_candidate = state
        .as_ref()
        .and_then(|state| state.pipeline.get("rescue_candidate").cloned());
    let mut run_projection = cli_job_run_to_json_with_activity_provenance(
        &effort_evidence,
        &run,
        state.as_ref(),
        url.as_deref(),
    );
    // [ORB-13899] The same audit scan holds the invocation's child: its live
    // progress, and the output reference a failed step never checkpoints.
    run_projection["agent_invocation"] = serde_json::to_value(agent_invoke_result(
        &run,
        state.as_ref(),
        provider_processes.last(),
    ))
    .unwrap_or(Value::Null);
    // Which catalog layer answered for each reference this run's job makes,
    // and what that layer shadowed. A plugin activity a workspace file
    // overrides is otherwise invisible (plugins design §8).
    let mut doc = json!({
        "run": run_projection,
        "recovery_attempts": recovery.to_json(),
        "tag_instructions": tag_instructions.to_json(),
        "catalog_layers": catalog_layers
            .iter()
            .map(CatalogReferenceLayer::to_json)
            .collect::<Vec<_>>(),
        "pipeline_state": state,
        "rescue_candidate": rescue_candidate,
        // The steps the view below renders, whichever source answered. The
        // record's own `run.steps` stay exactly as stored, so a caller can
        // still tell the two apart [ORB-12113].
        "steps": display_steps_with_agent_execution_to_json(&display_steps, state.as_ref(), &effort_evidence),
        "steps_source": steps_source.as_str(),
        // What a drain's leaves did; null for any run that is not a drain.
        // The drain's own `state` only says the coordinator ran.
        "drain_summary": drain_summary.as_ref().map(|summary| summary.to_json()),
        "waiting_on_lock_holders": lock_holders,
        "root_cause": root_causes.first().map(RunFailureCause::to_leaf_json),
        "additional_root_causes": root_causes
            .get(1..)
            .unwrap_or_default()
            .iter()
            .map(RunFailureCause::to_leaf_json)
            .collect::<Vec<_>>(),
        // The same projection the registered/MCP run-show surface emits, so
        // both readers name a live child identically [ORB-11752].
        "provider_processes": provider_processes
            .iter()
            .map(RunProviderProcess::to_json)
            .collect::<Vec<_>>(),
        // Every script-step attempt, refusals included.
        "script_executions": script_executions
            .iter()
            .map(RunScriptExecution::to_json)
            .collect::<Vec<_>>(),
    });

    pull.append_pull_drain_sections(&mut doc);

    let mut header =
        run_header_text_with_lock_holders(&run, state.as_ref(), &lock_holders, url.as_deref());
    if checkpoint_unreadable {
        header.push_str(&format!(
            "\n{} pipeline checkpoint unreadable; pipeline state omitted",
            crate::output::common::color::bold("Warning:"),
        ));
    }
    if let Some(mode) = auto_concurrency {
        header.push_str(&format!(
            "\nAuto-concurrency: {} ({}; host currently {}).",
            mode["mode"].as_str().unwrap_or("off"),
            mode["source"].as_str().unwrap_or("configured"),
            mode["configured_mode"].as_str().unwrap_or("off")
        ));
        doc["run"]["auto_concurrency"] = mode;
    }
    // What limited the drain's admission passes, local or pull.
    if let Some(counters) = state
        .as_ref()
        .and_then(|state| state.pull_drain.drain_pass_counters.as_ref())
    {
        header.push_str(&format!(
            "\n{} {}",
            crate::output::common::color::bold("Admission passes:"),
            counters.describe()
        ));
    }

    for launch in &effort_evidence {
        let effort = launch
            .execution
            .as_ref()
            .map(|identity| {
                identity
                    .reasoning_effort
                    .map(|effort| effort.to_string())
                    .unwrap_or_else(|| "provider default".into())
            })
            .unwrap_or_else(|| "unknown".into());
        header.push_str(&format!(
            "\nReasoning effort ({}): {} · {} · {}",
            launch.activity_id,
            launch
                .execution
                .as_ref()
                .and_then(|identity| identity.crew.as_deref())
                .unwrap_or("unknown crew"),
            launch.model.as_deref().unwrap_or("unknown model"),
            effort
        ));
    }
    if let Some(record) = &rescue_candidate
        && let Some(reference) = record["rescue_ref"].as_str()
    {
        header.push_str(&format!(
            "\n{} {} (commit {})",
            crate::output::common::color::bold("Rescue candidate:"),
            reference,
            record["rescue_commit"].as_str().unwrap_or_default(),
        ));
    }
    if let Some(line) = review_layer_line(
        &doc["run"]["review_layer"],
        pull.pull_claim
            .as_ref()
            .map(|claim| claim.owner_review_timing),
    ) {
        header.push('\n');
        header.push_str(&line);
    }

    for attempt in &recovery.attempts {
        header.push_str(&format!(
            "\nRecovery: {} at {}: {}{}",
            attempt.recovery_activity,
            attempt.failed_step_id,
            attempt.outcome,
            attempt
                .diagnostic
                .as_ref()
                .map(|detail| format!("; {detail}"))
                .unwrap_or_default(),
        ));
    }
    // The run's final-recovery decision and whose fault it found the failure
    // to be; `pipeline_state.final_recovery` carries the same in JSON.
    if let Some(checkpoint) = state
        .as_ref()
        .and_then(|state| state.final_recovery.as_ref())
        && let Some(decision) = &checkpoint.decision
    {
        header.push_str(&format!(
            "\nFinal recovery: {} ({}); fault: {}{}",
            decision.kind(),
            checkpoint
                .outcome
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "not applied".to_string()),
            checkpoint.fault.fault,
            if checkpoint.fault.fault_evidence.is_empty() {
                String::new()
            } else {
                format!(" ({})", checkpoint.fault.fault_evidence)
            },
        ));
    }
    if let Some(assignments) = run
        .input
        .as_ref()
        .and_then(|input| input.get("experiment_capture"))
        .and_then(|capture| capture.get("assignments"))
        .and_then(Value::as_array)
    {
        for assignment in assignments {
            header.push_str(&format!(
                "\nExperiment: {} / {} segment {} ({}, overlay {})",
                assignment["experiment"].as_str().unwrap_or("?"),
                assignment["arm"].as_str().unwrap_or("?"),
                assignment["segment"],
                if assignment["live"].as_bool() == Some(true) {
                    "live"
                } else {
                    "shadow"
                },
                assignment["digest"].as_str().unwrap_or("?"),
            ));
        }
    }
    for line in tag_instructions.header_lines() {
        header.push('\n');
        header.push_str(&line);
    }
    let mut displayed_build_budget_waits = HashSet::new();
    for process in &provider_processes {
        let Some(waits) = &process.build_budget_waits else {
            continue;
        };
        if waits.count == 0 && waits.deadline_extension_ms == 0 {
            continue;
        }
        let line = format!(
            "Build-budget waits ({}): count {}; total {:.3}s; longest {:.3}s; queued wall {:.3}s; deadline extended {:.3}s.",
            process.step_id.as_deref().unwrap_or("agent"),
            waits.count,
            waits.total_ms as f64 / 1000.0,
            waits.longest_ms as f64 / 1000.0,
            waits.queued_wall_ms as f64 / 1000.0,
            waits.deadline_extension_ms as f64 / 1000.0,
        );
        if displayed_build_budget_waits.insert(line.clone()) {
            header.push('\n');
            header.push_str(&line);
        }
    }
    for id in orbit_core::app::run::job_run_task_ids(&run) {
        let title = task_titles.get(&id);
        header.push_str(&format!(
            "\n{} {}{}",
            crate::output::common::color::bold("Task:"),
            id,
            title.map(|title| format!(" — {title}")).unwrap_or_default()
        ));
    }
    let cause_lines = format_root_cause_lines(&root_causes);
    if !cause_lines.is_empty() {
        header.push('\n');
        header.push_str(&cause_lines.join("\n"));
    }
    if let Some(leaf) = resumable_leaf {
        header.push_str(&format!("\n{} the failed work ran in {leaf}; `orbit job resume {leaf}` retries it, while resuming this run only re-checks that child's failure", crate::output::common::color::bold("Resume:")));
    }
    if let Some(state) = &state {
        header.push_str(&format!(
            "\n{} iteration={} step_outputs={} updated_at={}",
            crate::output::common::color::bold("Pipeline:"),
            state.iteration,
            state.step_output_entries().count(),
            state.updated_at.to_rfc3339(),
        ));
    }
    if let Some(claim) = &pull.pull_claim {
        header.push_str(&format!(
            "\n{} {}",
            crate::output::common::color::bold("Claim:"),
            claim.describe()
        ));
    }
    let provenance_lines =
        steps::activity_provenance_lines(&doc["run"]["activity_provenance"], &agent_activity_ids);
    if !provenance_lines.is_empty() {
        header.push('\n');
        header.push_str(&provenance_lines.join("\n"));
    }
    header.push_str(&catalog_layer_lines(&catalog_layers, verbose));
    header.push_str(&live_provider_process_lines(&provider_processes));
    for execution in &script_executions {
        header.push_str(&format!(
            "\n{} {}",
            crate::output::common::color::bold("Script:"),
            execution.describe()
        ));
    }
    header.push_str(&agent_invocation_lines(&doc["run"]["agent_invocation"]));
    header.push_str(&super::security_summary::security_alert_sweep_lines(
        &run,
        state.as_ref(),
    ));
    for line in super::worktree_gc::worktree_gc_lines(state.as_ref()) {
        header.push('\n');
        header.push_str(&line);
    }
    let exclusion_lines = format_backlog_exclusion_lines(state.as_ref());
    if !exclusion_lines.is_empty() {
        header.push('\n');
        header.push_str(&exclusion_lines.join("\n"));
    }
    if let Some(summary) = &drain_summary {
        let waiting_formatter = WaitingTaskFormatter::new(waiting_tasks);
        header.push('\n');
        header.push_str(&leaf_lines(summary, run.state, &waiting_formatter).join("\n"));
    } else if let Some(pass) = state
        .as_ref()
        .and_then(|state| state.pull_drain.drain_last_pass.as_ref())
    {
        // A pull drain records its passes too, without a leaf summary.
        let waiting_formatter = WaitingTaskFormatter::new(waiting_tasks);
        for line in pass_throttle_line(pass)
            .into_iter()
            .chain(pass_refusal_line(pass))
            .chain(pass_waiting_lines(pass, &waiting_formatter))
        {
            header.push('\n');
            header.push_str(&line);
        }
    }
    if let Some(state) = &state {
        let forecast = json!(
            state
                .pull_drain
                .drain_last_pass
                .as_ref()
                .and_then(|pass| pass.admission_forecast.as_ref())
        );
        let last_forecast = json!(state.pull_drain.drain_last_forecast);
        let origin = super::admission_forecast::ForecastOrigin {
            run_id: Some(&run.run_id),
            ended: run.state.is_terminal(),
            none_live: false,
        };
        for line in
            super::admission_forecast::admission_lines(&forecast, &last_forecast, &origin, |id| {
                task_titles.get(id).cloned()
            })
        {
            header.push('\n');
            header.push_str(&line);
        }
    }
    if let Some(pass) = state
        .as_ref()
        .and_then(|state| state.pull_drain.drain_last_pass.as_ref())
    {
        if let Some(error) = &pass.last_pass_error {
            // A local drain records a pass only when it deferred it on a busy
            // task-store lock; the next pass retries.
            let label = if run.job_id == orbit_core::app::run::PULL_DRAIN_JOB {
                "Pull pass warning:"
            } else {
                "Admission pass deferred:"
            };
            header.push_str(&format!(
                "\n{} consecutive_failures={} last_pass_error={error}",
                crate::output::common::color::bold(label),
                pass.consecutive_pass_failures,
            ));
        }
        if pass.degraded {
            header.push_str("\nDrain degraded: new admissions stopped; settlement retries continue. Fix the cause, run `orbit run auto --stop`, and start a new drain once this one ends.");
        }
    }
    // [ORB-15639] An open or half-open pull breaker: its failures, their
    // classes and when it next admits a probe claim. Absent while closed.
    if let Some(breaker) = &pull.pull_breaker {
        header.push_str(&format!(
            "\n{} {}",
            crate::output::common::color::bold("Breaker:"),
            breaker.describe(chrono::Utc::now())
        ));
    }
    header.push_str(&claimed_leaf_lines(
        &run,
        state.as_ref(),
        &pull.claimed_leaves,
    ));
    for refused in &pull.refused_settlements {
        header.push_str(&format!(
            "\n{} {}",
            crate::output::common::color::bold("Settlement refused:"),
            refused.describe()
        ));
    }
    if let Some(window) = &pull.crew_window {
        for line in window.describe() {
            header.push_str(&format!(
                "\n{} {line}",
                crate::output::common::color::bold("Crews:")
            ));
        }
    }
    if steps_source == StepSource::Audit && !display_steps.records.is_empty() {
        header.push_str(&format!(
            "\n{} reconstructed from the run audit trail; the run record holds no per-step history",
            crate::output::common::color::bold("Steps:"),
        ));
    }
    if let Some(evidence) = worker_limit {
        header.push_str(&format!(
            "\nWorker resource limit (watchdog): {} bytes, {} processes; error=worker_resource_limit",
            evidence.memory_bytes, evidence.process_count,
        ));
        for process in &evidence.top_processes {
            header.push_str(&format!(
                "\n  {} (pid {}): {} bytes",
                process.command, process.pid, process.memory_bytes
            ));
        }
        doc["worker_resource_limit"] = serde_json::to_value(evidence).map_err(|error| {
            OrbitError::Execution(format!("serialize worker resource evidence: {error}"))
        })?;
    }
    header.push('\n');

    Ok(Payload::blocks(
        doc,
        vec![
            Block::prose(header),
            Block::table(step_summary_table(&display_steps.records)),
        ],
    )
    .into())
}

/// The `Review:` line: the review layer before landing the run captured and
/// what decided it [ORB-16572]. Pull supervisors display their local config
/// separately because the owner decides each claim's review; legacy claimed
/// leaves without a persisted `review_layer` fall back to `pull_claim`.
fn review_layer_line(
    layer: &Value,
    claimed_owner_timing: Option<orbit_types::workflow::ReviewTiming>,
) -> Option<String> {
    if let Some(timing) = layer["timing"].as_str() {
        let origin = match layer["source"].as_str().unwrap_or("unknown") {
            orbit_types::workflow::REVIEW_RUN_OPTION_SOURCE => "run option --review".to_string(),
            "claim" => "the owner's claim".to_string(),
            source => format!("workspace policy, {source}"),
        };
        if layer["scope"].as_str() == Some(orbit_core::app::run::SUPERVISOR_LOCAL_REVIEW_SCOPE) {
            return Some(format!(
                "{} {timing} ({origin}; owner decides per claim)",
                crate::output::common::color::bold("Local review:")
            ));
        }
        return Some(format!(
            "{} {timing} ({origin})",
            crate::output::common::color::bold("Review:")
        ));
    }
    let timing = claimed_owner_timing?;
    Some(format!(
        "{} {} (the owner's claim)",
        crate::output::common::color::bold("Review:"),
        timing.as_str()
    ))
}

/// A pull drain's graceful cancel and the claimed leaves it still carries.
/// Empty for every other run.
fn claimed_leaf_lines(
    run: &JobRun,
    state: Option<&PipelineState>,
    leaves: &[DrainClaimedLeaf],
) -> String {
    let mut lines = String::new();
    if !run.state.is_terminal()
        && let Some(cancel) = state.and_then(|state| state.pull_drain.drain_cancel.as_ref())
    {
        lines.push_str(&format!(
            "\n{} waiting for {} leaves (requested by {} at {}{}); `orbit run cancel {} --confirm --force` stops them",
            crate::output::common::color::bold("Cancelling:"),
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
        // [ORB-15194] A reviewing leaf's slot admits a replacement beside it.
        let reviewing = leaves
            .iter()
            .filter(|leaf| leaf.stage == ClaimedLeafStage::Reviewing)
            .count();
        lines.push_str(&format!(
            "\n{} {} running ({} implementing, {} reviewing)",
            crate::output::common::color::bold("Claimed leaves:"),
            leaves.len(),
            leaves.len() - reviewing,
            reviewing
        ));
        for leaf in leaves {
            lines.push_str(&format!("\n  {}", leaf.describe()));
        }
    }
    lines
}
