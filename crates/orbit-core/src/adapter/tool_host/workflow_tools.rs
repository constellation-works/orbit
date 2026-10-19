use std::collections::BTreeSet;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::governance::operation::OperationVerb;
use orbit_common::governance::workflow::WorkflowVerb;
use orbit_types::identity::normalize_optional_attribution_label;
use orbit_types::workflow::{JobRun, JobRunState, JobRunTrigger, PipelineState};
use serde_json::{Value, json};

use crate::application::job::{
    ActivityInvocationEvidence, JobRunListParams, SubmissionKind, job_run_to_json,
    job_run_to_json_with_activity_provenance, run_display_steps,
};
use crate::runtime::audit::run::{
    MAX_RECOVERY_ATTEMPTS, RECOVERY_FETCH_PER_RUN, RunExecutionProgress, RunProviderProcess,
    RunRecoveryAttempts,
};
use crate::runtime::audit::tag_instructions::RunTagInstructions;
use crate::{OrbitRuntime, ShipMode};

use super::input::{parse_optional_string_array_field, parse_string_array_field};
use super::json::serialize_error;

const DEFAULT_RUN_LIST_LIMIT: usize = 25;
const MAX_RUN_LIST_LIMIT: usize = 200;

/// Run a workflow operation. `trigger` is the provenance of a run this call
/// submits.
pub(super) fn dispatch(
    runtime: &OrbitRuntime,
    session: &orbit_types::tool::ToolSessionContext,
    verb: WorkflowVerb,
    input: Value,
    agent: Option<String>,
    model: Option<String>,
    trigger: JobRunTrigger,
) -> Result<Value, OrbitError> {
    if verb == WorkflowVerb::Auto {
        orbit_common::governance::workflow::validate_auto_concurrency_input(&input)?;
    }
    // Re-wraps the input the tool entry already parsed; see `parse_input`.
    let input = verb.spec().parse_input(input)?.into_value();
    match verb {
        WorkflowVerb::Auto => super::desktop::drain::control(
            runtime,
            input,
            trigger,
            &actor(runtime, agent.as_deref(), model.as_deref()),
        ),
        WorkflowVerb::RunList => list(runtime, input),
        WorkflowVerb::RunResume if input.get("source_repair").is_some() => {
            runtime.source_repair_request(session, &input)
        }
        WorkflowVerb::RunResume => resume(runtime, input, agent, model),
        WorkflowVerb::RunShow => show(runtime, input),
        WorkflowVerb::Ship => ship(runtime, input, agent, model, trigger),
    }
}

fn ship(
    runtime: &OrbitRuntime,
    input: Value,
    agent: Option<String>,
    model: Option<String>,
    trigger: JobRunTrigger,
) -> Result<Value, OrbitError> {
    let task_ids = parse_string_array_field(&input, "task_ids")?;
    let unique = task_ids.iter().collect::<BTreeSet<_>>();
    if unique.len() != task_ids.len() {
        return Err(OrbitError::InvalidInput(
            "`task_ids` must not contain duplicates".to_string(),
        ));
    }
    let mode = match optional_string(&input, "mode")? {
        Some(raw) => ShipMode::parse(&raw)?,
        None => runtime
            .workspace_runtime_binding()
            .map_or(ShipMode::Pr, |binding| binding.ship_mode),
    };
    let base = optional_string(&input, "base")?;
    let allowed_crews = parse_optional_string_array_field(&input, "allowed_crews")?;
    let actor = actor(runtime, agent.as_deref(), model.as_deref());
    let claim_token = optional_string(&input, "claim_token")?;
    let review = review_layer(&input)?;
    let invoke = runtime.submit_ship_run_with_containment(
        mode,
        base.as_deref(),
        &task_ids,
        // [ORB-11187] Completion authority is an operator decision made at the
        // CLI; this tool surface does not advertise or accept it.
        crate::application::workflow::CompletionPolicy::Review,
        &allowed_crews,
        Some(&actor),
        claim_token.as_deref(),
        trigger,
        // Strict worker containment is a CLI-only flag.
        false,
        review,
    )?;
    let mut shipped = json!({
        "workflow": "ship",
        "job_id": invoke.job_name,
        "run_id": invoke.run_id,
        "state": if invoke.queued { "queued" } else { "submitted" },
        "submitted_at": invoke.submitted_at,
    });
    runtime
        .submission_notices(SubmissionKind::Ship {
            explicit: !task_ids.is_empty(),
        })
        .insert_into(&mut shipped);
    Ok(shipped)
}

/// The optional `review` layer a ship or drain start adds [ORB-16572].
pub(super) fn review_layer(
    input: &Value,
) -> Result<Option<orbit_types::workflow::ReviewLayer>, OrbitError> {
    optional_string(input, "review")?
        .map(|raw| {
            raw.parse()
                .map_err(|message: &str| OrbitError::InvalidInput(message.into()))
        })
        .transpose()
}

/// One run: the record, the child Runs it dispatched, and what it is doing
/// now.
///
/// [ORB-10971] The `JobRun` row alone cannot answer "what did this run submit,
/// and is it still waiting on it" — that lives in the run's `PipelineState`.
/// Reading it here is what keeps the MCP surface agreeing with the CLI and the
/// dashboard about lineage instead of each reader seeing a different half of
/// the truth. An unreadable state degrades to an empty list rather than
/// failing the read.
fn show(runtime: &OrbitRuntime, input: Value) -> Result<Value, OrbitError> {
    let id = orbit_common::protocol::tool_input::required_string(&input, &["id"], "id")?;
    let run = runtime.show_job_run(&id)?;
    let state = runtime.read_run_state(&run.run_id).ok().flatten();
    let recovery = runtime.collect_run_recovery_attempts(&run.run_id).ok();
    // An unreadable invocation store shows every activity as `unavailable`,
    // never as a failed read of the run.
    let evidence = runtime
        .activity_invocation_evidence(&run.run_id)
        .unwrap_or_default();
    let mut value = run_json_enriched(
        runtime,
        &run,
        state.as_ref(),
        Some(&evidence),
        recovery.as_ref(),
    )?;
    let progress = runtime
        .collect_run_execution_progress(&run.run_id)
        .unwrap_or_else(|_| RunExecutionProgress::unavailable());
    // [ORB-13899] One run, so the invocation also gets its child's evidence:
    // live progress, and the output reference a failed step never checkpoints.
    value["agent_invocation"] = serde_json::to_value(crate::application::job::agent_invoke_result(
        &run,
        state.as_ref(),
        progress.provider_processes.last(),
    ))
    .map_err(serialize_error("serialize agent invocation result"))?;
    // The parsed answer already contains the useful result fields. Its raw
    // final message repeats the provider envelope, and preview repeats the
    // beginning of that same captured stream. Keep the durable blob refs for
    // callers that need the complete output.
    if let Some(invocation) = value["agent_invocation"].as_object_mut()
        && invocation.get("answer").is_some_and(Value::is_object)
    {
        invocation.remove("preview");
        invocation.remove("preview_truncated");
        if let Some(answer) = invocation.get_mut("answer").and_then(Value::as_object_mut) {
            answer.remove("final_message");
        }
    }
    value["execution_progress"] = execution_progress_json(&progress);
    // Which trusted tag instructions each activity's prompt carried, so a
    // reader can tell why an agent followed them; `unavailable` when the
    // audit trail cannot be read.
    value["tag_instructions"] = runtime
        .collect_run_tag_instructions(&run.run_id)
        .unwrap_or_else(|_| RunTagInstructions::unavailable())
        .to_json();
    Ok(value)
}

/// [ORB-11752] What the run is doing right now, for the reader that asked
/// about exactly one run.
///
/// Without it this surface can report a `running` wrapper PID and nothing
/// else, so a healthy implementation agent and an abandoned wrapper look
/// identical here and an orchestrator has to fall back to an operator command.
/// `list` deliberately does not carry it: the evidence costs an audit scan plus
/// a liveness probe per open child, which is the right price for one deliberate
/// read and the wrong one for a 200-run page.
///
/// The projection is identifiers, timestamps and process facts plus each
/// child's newest assistant message, which the engine bounded and redacted
/// before persisting it [ORB-13899]. An
/// unreadable audit trail degrades to `unavailable` rather than failing the
/// durable run read, as every other evidence field here does.
fn execution_progress_json(progress: &RunExecutionProgress) -> Value {
    json!({
        "state": progress.state,
        "active_step": progress.active_step.as_ref().map(|step| json!({
            "step_id": step.step_id,
            "step_index": step.step_index,
            "started_at": step.started_at.map(|value| value.to_rfc3339()),
        })),
        "provider_processes": {
            "limit": progress.limit,
            "truncated": progress.truncated,
            "items": progress
                .provider_processes
                .iter()
                .map(RunProviderProcess::to_json)
                .collect::<Vec<_>>(),
        },
    })
}

fn list(runtime: &OrbitRuntime, input: Value) -> Result<Value, OrbitError> {
    let state = optional_string(&input, "state")?;
    let terminal_only = state.as_deref() == Some("terminal");
    let state = state
        .filter(|value| value != "terminal")
        .map(|value| {
            JobRunState::from_str(&value)
                .map_err(|error| OrbitError::InvalidInput(format!("`state` {error}")))
        })
        .transpose()?;
    let since = optional_string(&input, "since")?
        .map(|value| {
            DateTime::parse_from_rfc3339(&value)
                .map(|value| value.with_timezone(&Utc))
                .map_err(|error| OrbitError::InvalidInput(format!("`since` {error}")))
        })
        .transpose()?;
    let runs = runtime.list_job_runs(JobRunListParams {
        job_id: optional_string(&input, "job_id")?,
        state,
        terminal_only,
        since,
        limit: Some(parse_limit(&input)?),
        ..Default::default()
    })?;
    // Default list stays the enriched projection. A summary mode, if added,
    // must be an explicit opt-in and cannot replace this path [ORB-11625].
    let (items, _reads) = project_workflow_run_list(runtime, &runs)?;
    Ok(json!({ "items": items }))
}

/// Query counts for one list-page projection. Reconciliation reads that happen
/// inside `list_job_runs` are excluded: they are not this enrichment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RunListProjectionReads {
    pub pipeline_state_queries: usize,
    pub recovery_event_queries: usize,
    pub recovery_presence_queries: usize,
    pub per_run_recovery_fetch_limit: usize,
    pub per_run_recovery_projection_limit: usize,
}

fn project_workflow_run_list(
    runtime: &OrbitRuntime,
    runs: &[JobRun],
) -> Result<(Vec<Value>, RunListProjectionReads), OrbitError> {
    let reads = RunListProjectionReads {
        pipeline_state_queries: usize::from(!runs.is_empty()),
        recovery_event_queries: 2 * usize::from(!runs.is_empty()),
        recovery_presence_queries: usize::from(!runs.is_empty()),
        per_run_recovery_fetch_limit: RECOVERY_FETCH_PER_RUN,
        per_run_recovery_projection_limit: MAX_RECOVERY_ATTEMPTS,
    };
    if runs.is_empty() {
        return Ok((Vec::new(), reads));
    }

    let run_ids = runs
        .iter()
        .map(|run| run.run_id.clone())
        .collect::<Vec<_>>();
    let states = runtime.read_job_run_list_states(runs).unwrap_or_default();
    let recoveries = runtime
        .collect_run_recovery_attempts_for_runs(&run_ids)
        .map(|page| page.by_run_id)
        .unwrap_or_default();
    let items = runs
        .iter()
        .map(|run| {
            run_json_enriched(
                runtime,
                run,
                states.state(&run.run_id),
                None,
                recoveries.get(&run.run_id),
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((items, reads))
}

fn resume(
    runtime: &OrbitRuntime,
    input: Value,
    agent: Option<String>,
    model: Option<String>,
) -> Result<Value, OrbitError> {
    let id = orbit_common::protocol::tool_input::required_string(&input, &["id"], "id")?;
    let actor = actor(runtime, agent.as_deref(), model.as_deref());
    let claim_token = optional_string(&input, "claim_token")?;
    let invoke = runtime.submit_resume_run(&id, Some(&actor), claim_token.as_deref())?;
    Ok(json!({
        "workflow": "resume",
        "job_id": invoke.job_name,
        "run_id": invoke.run_id,
        "retry_source_run_id": id,
        "state": if invoke.queued { "queued" } else { "submitted" },
        "submitted_at": invoke.submitted_at,
    }))
}

fn optional_string(input: &Value, field: &str) -> Result<Option<String>, OrbitError> {
    orbit_common::protocol::tool_input::optional_string(input, field)
}

fn actor(runtime: &OrbitRuntime, agent: Option<&str>, model: Option<&str>) -> String {
    normalize_optional_attribution_label(model.or(agent), model)
        .unwrap_or_else(|| runtime.actor_label().to_string())
}

fn parse_limit(input: &Value) -> Result<usize, OrbitError> {
    let Some(value) = input.get("limit") else {
        return Ok(DEFAULT_RUN_LIST_LIMIT);
    };
    let limit = value.as_u64().ok_or_else(|| {
        OrbitError::InvalidInput("`limit` must be a positive integer".to_string())
    })?;
    if limit == 0 {
        return Err(OrbitError::InvalidInput(
            "`limit` must be at least 1".to_string(),
        ));
    }
    if limit > MAX_RUN_LIST_LIMIT as u64 {
        return Err(OrbitError::InvalidInput(format!(
            "`limit` must be at most {MAX_RUN_LIST_LIMIT}"
        )));
    }
    usize::try_from(limit).map_err(|_| OrbitError::InvalidInput("`limit` is too large".to_string()))
}

/// One run on the MCP surface: the shared run projection, the displayed steps
/// every run view chooses by one rule, and this surface's own additions.
///
/// The additions are the submitted `input` and the claimed worker's
/// `pid`/`pid_start_time`, which the shared projection leaves to each
/// surface, plus the drain and recovery evidence this surface has always
/// carried on every run.
fn run_json_enriched(
    runtime: &OrbitRuntime,
    run: &JobRun,
    state: Option<&PipelineState>,
    evidence: Option<&[ActivityInvocationEvidence]>,
    recovery: Option<&RunRecoveryAttempts>,
) -> Result<Value, OrbitError> {
    let mut value = match evidence {
        Some(evidence) => job_run_to_json_with_activity_provenance(run, state, evidence),
        None => job_run_to_json(run, state),
    };
    value["input"] = json!(run.input);
    value["pid"] = json!(run.pid);
    value["pid_start_time"] = json!(run.pid_start_time);
    // [ORB-12255] A worker that left the record empty still has its steps in
    // the audit trail; `steps_source` names which one answered.
    let steps = run_display_steps(run, || {
        runtime
            .collect_run_audit_steps(&run.run_id)
            .unwrap_or_default()
    });
    value["steps"] = steps.to_json();
    value["steps_source"] = json!(steps.source.as_str());
    // [ORB-13901] A drain's last admission pass, including the host resource
    // throttle that held it; null for every run that is not a drain.
    value["drain_last_pass"] =
        serde_json::to_value(state.and_then(|state| state.pull_drain.drain_last_pass.as_ref()))
            .map_err(serialize_error("serialize drain last pass"))?;
    // [ORB-14117] What an `--approve-proposed` drain approved and held.
    value["drain_approvals"] =
        serde_json::to_value(state.and_then(|state| state.pull_drain.drain_approvals.as_ref()))
            .map_err(serialize_error("serialize drain approvals"))?;
    // Recovery evidence remains separate from the run and step errors above:
    // a successful or failed recovery attempt never rewrites the original
    // workflow failure that triggered it. Older/unreadable audit trails remain
    // observable as `unavailable` rather than making existing run-show callers
    // fail their ordinary durable run read.
    value["recovery_attempts"] = match recovery {
        Some(attempts) => attempts.to_json(),
        None => json!({
            "state": "unavailable",
            "limit": MAX_RECOVERY_ATTEMPTS,
            "truncated": false,
            "items": [],
        }),
    };
    Ok(value)
}
