//! Shared JSON projection for persisted job runs.

use std::collections::HashMap;

use orbit_common::OrbitError;
use orbit_types::workflow::{
    CrewPurpose, FINAL_RECOVERY_CREWS_KEY, JobRun, JobRunState, JobRunStep, PipelineState,
    run_id_role,
};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::runtime::run_input::carried_task_ids;

use super::display_steps::RunDisplayStep;

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

/// Every activity crew draw the run froze, with the rule that chose it: the
/// purpose's own pool, the fallback an unset purpose keeps, or the pool key
/// the activity named. A draw that fell back to `workflow.default_crew` says
/// so.
fn activity_crew_draws_json(state: Option<&PipelineState>) -> Value {
    Value::Array(
        state
            .into_iter()
            .flat_map(|state| &state.activity_crew_draws)
            .map(|(key, draw)| {
                let rule = if draw.source.starts_with("workflow.default_crew") {
                    "default_crew"
                } else if CrewPurpose::from_purpose_key(key).is_some() {
                    if draw.source.starts_with(key.as_str()) {
                        "purpose_pool"
                    } else {
                        "purpose_unset_fallback"
                    }
                } else if key == FINAL_RECOVERY_CREWS_KEY {
                    "final_recovery_pool"
                } else {
                    "complexity_pool"
                };
                json!({
                    "key": key,
                    "crew": draw.crew,
                    "rule": rule,
                    "source": draw.source,
                })
            })
            .collect(),
    )
}

/// Task identities recorded in submitted run input, without any task-store reads.
/// Array and singular bindings share the dashboard's sorted, deduplicated shape.
pub fn job_run_task_ids(run: &JobRun) -> Vec<String> {
    let Some(input) = run.input.as_ref() else {
        return Vec::new();
    };
    carried_task_ids(input).into_iter().collect()
}

/// The step whose error explains a run's outcome [ORB-13016].
///
/// A successful run has none. Otherwise the newest step that is not skipped
/// wins, preferring one that recorded an error: a skipped step's `when:`
/// reason is why that step did not run, not why the run ended, so it stays on
/// the step.
pub fn run_error_step(run: &JobRun) -> Option<&JobRunStep> {
    if run.state == JobRunState::Success {
        return None;
    }
    let ran = || {
        run.steps
            .iter()
            .rev()
            .filter(|step| step.state != JobRunState::Skipped)
    };
    ran()
        .find(|step| step.error_code.is_some() || step.error_message.is_some())
        .or_else(|| ran().next())
        .or_else(|| run.steps.last())
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
    let task_ids = job_run_task_ids(run);
    let last = run.steps.last();
    let error_step = run_error_step(run);
    let child_dispatches = serde_json::to_value(
        state
            .map(|state| state.child_dispatches.as_slice())
            .unwrap_or_default(),
    )
    .unwrap_or_else(|_| Value::Array(Vec::new()));
    let drain_worker_limit = state
        .and_then(|state| state.pull_drain.drain_worker_limit.as_ref())
        .and_then(|limit| serde_json::to_value(limit).ok())
        .unwrap_or(Value::Null);
    let drain_admissions_stop = state
        .and_then(|state| state.pull_drain.drain_admissions_stop.as_ref())
        .and_then(|stop| serde_json::to_value(stop).ok())
        .unwrap_or(Value::Null);
    let drain_window_extension = state
        .and_then(|state| state.pull_drain.drain_window_extension.as_ref())
        .and_then(|extension| serde_json::to_value(extension).ok())
        .unwrap_or(Value::Null);
    let drain_cancel = state
        .and_then(|state| state.pull_drain.drain_cancel.as_ref())
        .and_then(|cancel| serde_json::to_value(cancel).ok())
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
    let agent_invocation =
        crate::application::job::agent_invoke_result(run, state_for_agent_result, None)
            .and_then(|result| serde_json::to_value(result).ok())
            .unwrap_or(Value::Null);

    let mut value = json!({
        "agent_invocation": agent_invocation,
        "child_dispatches": child_dispatches,
        "drain_worker_limit": drain_worker_limit,
        "drain_admissions_stop": drain_admissions_stop,
        "drain_cancel": drain_cancel,
        "run_id": run.run_id,
        "executed_on": run.executed_on,
        "run_role": run_id_role(&run.run_id).map(|role| role.to_string()),
        "trigger": state_for_agent_result.and_then(|state| state.trigger.as_ref()),
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
        "error_code": error_step.and_then(|step| step.error_code.as_deref()),
        "error_message": error_step.and_then(|step| step.error_message.as_deref()),
        "knowledge_metrics": run.knowledge_metrics,
        "requested_crew": requested_crew,
        "resolved_crew": run.resolved_crew,
        "crew_model": run.crew_model,
        "resolved_run_crew": {
            "crew": run.resolved_crew,
            "model": run.crew_model,
        },
        "activity_crew_draws": activity_crew_draws_json(state_for_agent_result),
        // The record's own steps, exactly as stored. A surface that renders
        // a run's steps chooses them with `run_display_steps`.
        "steps": run.steps.iter().map(|step| RunDisplayStep::from_job_step(step).to_json()).collect::<Vec<_>>(),
        "created_at": run.created_at.to_rfc3339(),
    });
    if run.job_id == "workspace_auto_pipeline"
        && let Some(input) = run.input.as_ref()
        && let Some(mode) = input.get("auto_concurrency").and_then(Value::as_str)
    {
        value["auto_concurrency"] = json!({
            "mode": mode,
            "source": input.get("auto_concurrency_source").and_then(Value::as_str).unwrap_or("explicit"),
        });
    }
    // [ORB-16572] The captured review layer and what decided it: workspace
    // policy, or the run's `--review` option.
    if let Some(admission) = run
        .input
        .as_ref()
        .and_then(|input| orbit_types::workflow::ReviewAdmission::from_run_input(input).ok())
        .flatten()
    {
        value["review_layer"] = crate::application::review::review_layer_json(&admission);
    }
    value["task_ids"] = json!((!task_ids.is_empty()).then_some(task_ids));
    value["drain_window_extension"] = drain_window_extension;
    // [ORB-14777] Pass-listed variables the submitting process did not hold.
    // Added outside the literal: it is at `json!`'s recursion limit.
    value["env_pass_unset"] = json!(
        state_for_agent_result
            .map(|state| state.env_pass_unset.as_slice())
            .unwrap_or_default()
    );
    // Crews whose `env` sources the submitting process did not hold. Names
    // only.
    value["crew_env_unset"] = json!(
        state_for_agent_result
            .map(|state| state.crew_env_unset.as_slice())
            .unwrap_or_default()
    );
    // [ORB-15202] The leaf a cascaded failure echoes; null for a run that
    // failed on its own.
    value["root_cause"] = json!(state_for_agent_result.and_then(|state| state.root_cause.as_ref()));
    value
}

/// Pipeline states for one run-list page, read in one store round-trip.
///
/// Every surface that lists runs projects each row with its own state, so a
/// listing reports the same child dispatches, waiting reasons, trigger and
/// drain controls as the run itself. The read also covers each listed run's
/// retry source, whose dispatches a lineage view needs even when that source
/// is not on the page.
#[derive(Debug, Default)]
pub struct JobRunListStates {
    states: HashMap<String, Option<PipelineState>>,
}

impl JobRunListStates {
    /// The persisted state of a run the page read, if it has one.
    pub fn state(&self, run_id: &str) -> Option<&PipelineState> {
        self.states.get(run_id).and_then(Option::as_ref)
    }

    /// Each listed run's state, in listing order.
    pub fn for_runs<'a>(&'a self, runs: &[JobRun]) -> Vec<Option<&'a PipelineState>> {
        runs.iter().map(|run| self.state(&run.run_id)).collect()
    }

    /// The shared row for a listed run, projected with its state.
    pub fn row(&self, run: &JobRun) -> Value {
        job_run_to_json(run, self.state(&run.run_id))
    }
}

impl OrbitRuntime {
    /// Read the pipeline states a run-list page projects: the listed runs'
    /// and their retry sources'. An empty page reads nothing. A failed read
    /// is returned, so each surface keeps its own policy for a listing
    /// without states.
    pub fn read_job_run_list_states(
        &self,
        runs: &[JobRun],
    ) -> Result<JobRunListStates, OrbitError> {
        let mut run_ids = runs
            .iter()
            .map(|run| run.run_id.clone())
            .chain(
                runs.iter()
                    .filter_map(|run| run.retry_source_run_id.clone()),
            )
            .collect::<Vec<_>>();
        if run_ids.is_empty() {
            return Ok(JobRunListStates::default());
        }
        run_ids.sort_unstable();
        run_ids.dedup();
        Ok(JobRunListStates {
            states: self.read_run_states(&run_ids)?,
        })
    }
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
