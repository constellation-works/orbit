//! Shared JSON projection for persisted job runs.

use orbit_types::workflow::{JobRun, PipelineState, run_id_role};
use serde_json::{Value, json};

/// Durable provider/model evidence for one completed agent invocation.
///
/// This is intentionally separate from a run's resolved crew: the crew is the
/// routing decision made before dispatch, while this evidence says what the
/// provider actually reported after an activity ran.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivityInvocationEvidence {
    pub activity_id: String,
    pub provider: String,
    pub model: Option<String>,
}

/// Project a job run and its optional persisted state for operator-facing APIs.
///
/// Child-dispatch lineage is historical and remains visible after a run reaches
/// a terminal state, and so are an operator-set worker ceiling [ORB-11253] and
/// admissions stop [ORB-11283]: they are the evidence of what the run was
/// actually admitting under, which a reader needs exactly when explaining a
/// finished drain. Waiting reasons are momentary, so terminal runs omit them.
/// Presentation adapters may add intentionally surface-specific fields.
///
/// `run_role` is the run id's own claim about how the run was submitted, and is
/// null for ids minted before role markers existed [ORB-12111]. It never
/// overrides `child_dispatches`, which remains the record of who dispatched
/// whom.
pub fn job_run_to_json(run: &JobRun, state: Option<&PipelineState>) -> Value {
    let last = run.steps.last();
    let child_dispatches = serde_json::to_value(
        state
            .map(|state| state.child_dispatches.as_slice())
            .unwrap_or_default(),
    )
    .unwrap_or_else(|_| Value::Array(Vec::new()));
    let drain_worker_limit = state
        .and_then(|state| state.drain_worker_limit.as_ref())
        .and_then(|limit| serde_json::to_value(limit).ok())
        .unwrap_or(Value::Null);
    let drain_admissions_stop = state
        .and_then(|state| state.drain_admissions_stop.as_ref())
        .and_then(|stop| serde_json::to_value(stop).ok())
        .unwrap_or(Value::Null);
    // Read before the terminal filter below: an invocation's result is most
    // interesting *after* the run finishes.
    let state_for_agent_result = state;
    let state = (!run.state.is_terminal()).then_some(state).flatten();
    let waiting_on_deps = state
        .and_then(|state| state.waiting_on_deps.as_ref())
        .filter(|values| !values.is_empty());
    let waiting_on_locks = state
        .and_then(|state| state.waiting_on_locks.as_ref())
        .filter(|values| !values.is_empty());
    let requested_crew = run
        .input
        .as_ref()
        .and_then(|input| input.get("crew"))
        .and_then(Value::as_str);

    // [ORB-11354] An agent invocation's answer is the reason its run exists,
    // so the shared projection carries it rather than making each surface dig
    // it out of the step output. `None` for every other job.
    let agent_invocation = crate::application::job::agent_invoke_result(
        run,
        state_for_agent_result.map(|state| &state.step_outputs),
    )
    .and_then(|result| serde_json::to_value(result).ok())
    .unwrap_or(Value::Null);

    json!({
        "agent_invocation": agent_invocation,
        "child_dispatches": child_dispatches,
        "drain_worker_limit": drain_worker_limit,
        "drain_admissions_stop": drain_admissions_stop,
        "run_id": run.run_id,
        "run_role": run_id_role(&run.run_id).map(|role| role.to_string()),
        "job_id": run.job_id,
        "attempt": run.attempt,
        "state": run.state.to_string(),
        "waiting_on_deps": waiting_on_deps,
        "waiting_on_locks": waiting_on_locks,
        "scheduled_at": run.scheduled_at.to_rfc3339(),
        "started_at": run.started_at.map(|value| value.to_rfc3339()),
        "finished_at": run.finished_at.map(|value| value.to_rfc3339()),
        "duration_ms": run.duration_ms,
        "retry_source_run_id": run.retry_source_run_id,
        "exit_code": last.and_then(|step| step.exit_code),
        "agent_response_json": last.and_then(|step| step.agent_response_json.as_ref()),
        "error_code": last.and_then(|step| step.error_code.as_deref()),
        "error_message": last.and_then(|step| step.error_message.as_deref()),
        "knowledge_metrics": run.knowledge_metrics,
        "requested_crew": requested_crew,
        "resolved_crew": run.resolved_crew,
        "crew_model": run.crew_model,
        "resolved_run_crew": {
            "crew": run.resolved_crew,
            "model": run.crew_model,
        },
        "steps": run.steps.iter().map(|step| json!({
            "step_index": step.step_index,
            "target_type": step.target_type.to_string(),
            "target_id": step.target_id,
            "state": step.state.to_string(),
            "started_at": step.started_at.map(|value| value.to_rfc3339()),
            "finished_at": step.finished_at.map(|value| value.to_rfc3339()),
            "duration_ms": step.duration_ms,
            "exit_code": step.exit_code,
            "agent_response_json": step.agent_response_json,
            "error_code": step.error_code,
            "error_message": step.error_message,
        })).collect::<Vec<_>>(),
        "created_at": run.created_at.to_rfc3339(),
    })
}

/// Add activity-level provider/model evidence to the stable job-run projection.
///
/// Missing evidence is explicit. It is not filled from the resolved run crew,
/// because that would turn a requested route into a false claim about token
/// usage. A deterministic workflow wrapper has no activity evidence of its
/// own.
pub fn job_run_to_json_with_activity_provenance(
    run: &JobRun,
    state: Option<&PipelineState>,
    evidence: &[ActivityInvocationEvidence],
) -> Value {
    let mut value = job_run_to_json(run, state);
    let activities = run
        .steps
        .iter()
        .filter(|step| step.target_type.to_string() == "activity")
        .map(|step| {
            let invocations = evidence
                .iter()
                .filter(|record| record.activity_id == step.target_id)
                .map(|record| {
                    json!({
                        "provider": record.provider,
                        "model": record.model,
                    })
                })
                .collect::<Vec<_>>();
            let status = if invocations.is_empty() {
                if step.started_at.is_some() {
                    "unavailable"
                } else {
                    "not_started"
                }
            } else {
                "recorded"
            };
            json!({
                "activity_id": step.target_id,
                "actual_status": status,
                "invocations": invocations,
            })
        })
        .collect::<Vec<_>>();
    value["activity_provenance"] = Value::Array(activities);
    value
}
