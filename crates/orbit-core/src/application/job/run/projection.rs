//! Shared JSON projection for persisted job runs.

use std::collections::HashMap;

use orbit_common::OrbitError;
use orbit_types::workflow::{
    ActivityDrawOrigin, CrewPurpose, FINAL_RECOVERY_CREWS_KEY, JobRun, JobRunState, JobRunStep,
    PipelineState, run_id_role,
};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::runtime::audit::run::{RunExecutionProgress, RunProviderProcess, RunRecoveryAttempts};
use crate::runtime::audit::run_logs::bounded_redacted_text;
use crate::runtime::run_input::carried_task_ids;

use super::display_steps::{RunDisplayStep, RunDisplaySteps};

/// Durable provider/model evidence for one completed agent invocation.
///
/// This is intentionally separate from a run's resolved crew: the crew is the
/// routing decision made before dispatch, while this evidence says what the
/// provider actually reported after an activity ran.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivityInvocationEvidence {
    pub execution: Option<orbit_types::telemetry::AgentExecutionIdentity>,
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
                let origin = draw.source.origin();
                let rule = if *origin == ActivityDrawOrigin::DefaultCrew {
                    "default_crew"
                } else if CrewPurpose::from_purpose_key(key).is_some() {
                    match origin {
                        ActivityDrawOrigin::ConfiguredPool { key: pool } if pool == key => {
                            "purpose_pool"
                        }
                        _ => "purpose_unset_fallback",
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
        "build": run.build,
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
        value["review_layer"] = crate::application::contracts::review_layer_json(&admission);
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
    value["crew_pool_warnings"] = json!(
        state
            .map(|state| state.pull_drain.pool_crew_exclusion_warnings())
            .unwrap_or_default()
    );
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
        project_job_run(
            run,
            JobRunProjectionProfile::ListRow,
            JobRunProjectionEvidence {
                state: self.state(&run.run_id),
                ..JobRunProjectionEvidence::default()
            },
        )
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
                        "crew": record.execution.as_ref().and_then(|identity| identity.crew.as_deref()),
                        "reasoning_effort": record.execution.as_ref().map(|identity| identity.reasoning_effort.map(|effort| effort.to_string())).unwrap_or(Some("unknown".to_string())),
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
    value["agent_executions"] = json!(evidence.iter().map(|record| json!({
        "activity_id": record.activity_id, "provider": record.provider, "model": record.model,
        "crew": record.execution.as_ref().and_then(|identity| identity.crew.as_deref()),
        "reasoning_effort": record.execution.as_ref().map(|identity| identity.reasoning_effort.map(|effort| effort.to_string())).unwrap_or(Some("unknown".to_string())),
    })).collect::<Vec<_>>());
    value["activity_provenance"] = Value::Array(activities);
    value
}

/// Named wire profiles for run inspection [ORB-24357].
///
/// Every profile preserves one established surface shape exactly: which
/// worker fields ride on the run object, where displayed steps sit, and
/// which drain/recovery sections the run object carries. Surfaces pick a
/// profile and keep only input parsing, evidence reads (with their own
/// degradation/logging) and envelope/text rendering; the JSON policy lives
/// in [`project_job_run`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobRunProjectionProfile {
    /// Full detail whose run object keeps its stored `steps`; displayed
    /// steps are a sibling the caller renders with
    /// [`display_steps_with_agent_execution_to_json`] (CLI `run show`).
    FullDetailStoredSteps,
    /// Full detail whose run object drops `steps`; displayed steps are a
    /// sibling (HTTP run detail, including the recorded-only variant).
    FullDetailSiblingSteps,
    /// Full detail whose run object carries the displayed steps,
    /// `steps_source`, `drain_last_pass`, `drain_approvals` and
    /// `recovery_attempts` (workflow tool show/list/observe).
    FullDetailEmbeddedSteps,
    /// History list row: provenance sections and `pid` only — no submitted
    /// input, no `pid_start_time` (CLI `run history`).
    ListHistory,
    /// Plain list row: the base projection with no worker fields and no
    /// provenance sections (HTTP run list via [`JobRunListStates::row`]).
    ListRow,
    /// Bounded summary: `id` alias plus `run_id`, core timing/state fields
    /// and `usage.state = unavailable` (desktop bounded read).
    BoundedSummary,
}

/// Evidence a surface gathered for one run, handed to [`project_job_run`].
///
/// Gathering stays with the caller because each profile promises a
/// different read/degradation contract (reconciling vs observed, warn vs
/// propagate); this struct is the shared input shape so no surface
/// re-derives the JSON from the pieces.
#[derive(Clone, Copy, Default)]
pub struct JobRunProjectionEvidence<'a> {
    /// The run's persisted pipeline state, when readable.
    pub state: Option<&'a PipelineState>,
    /// Activity invocation evidence. `Some` (even empty) renders the
    /// provenance sections; `None` renders the base projection without them.
    pub activity_evidence: Option<&'a [ActivityInvocationEvidence]>,
    /// Displayed steps chosen by `run_display_steps`; required only by
    /// [`JobRunProjectionProfile::FullDetailEmbeddedSteps`].
    pub display_steps: Option<&'a RunDisplaySteps>,
    /// Recovery evidence for the embedded profile; `None` renders the
    /// explicit `unavailable` shape.
    pub recovery: Option<&'a RunRecoveryAttempts>,
}

/// Add the claimed worker's identity and the submitted input a full-detail
/// run object carries on every surface.
fn add_run_worker_fields(value: &mut Value, run: &JobRun) {
    value["input"] = json!(run.input);
    value["pid"] = json!(run.pid);
    value["pid_start_time"] = json!(run.pid_start_time);
}

/// Drain pass health, kept visible after terminalization independently of
/// the live waiting-state fields the base projection omits then.
fn run_drain_last_pass_to_json(state: Option<&PipelineState>) -> Value {
    serde_json::to_value(state.and_then(|state| state.pull_drain.drain_last_pass.as_ref()))
        .unwrap_or(Value::Null)
}

/// Displayed steps beside their source, with recorded agent executions:
/// the step array every full-detail surface renders, embedded or sibling.
pub fn display_steps_with_agent_execution_to_json(
    steps: &RunDisplaySteps,
    state: Option<&PipelineState>,
    evidence: &[ActivityInvocationEvidence],
) -> Value {
    json!(
        steps
            .records
            .iter()
            .map(|step| step.to_json_with_agent_execution(state, evidence))
            .collect::<Vec<_>>()
    )
}

/// Project one run under a named profile; see [`JobRunProjectionProfile`].
pub fn project_job_run(
    run: &JobRun,
    profile: JobRunProjectionProfile,
    evidence: JobRunProjectionEvidence<'_>,
) -> Value {
    if profile == JobRunProjectionProfile::BoundedSummary {
        return json!({"id": run.run_id, "run_id": run.run_id, "job_id": run.job_id,
            "state": run.state, "attempt": run.attempt, "scheduled_at": run.scheduled_at,
            "started_at": run.started_at, "finished_at": run.finished_at,
            "created_at": run.created_at, "duration_ms": run.duration_ms,
            "usage": {"state": "unavailable"}});
    }
    let mut value = match evidence.activity_evidence {
        Some(activity) => job_run_to_json_with_activity_provenance(run, evidence.state, activity),
        None => job_run_to_json(run, evidence.state),
    };
    match profile {
        JobRunProjectionProfile::ListRow | JobRunProjectionProfile::BoundedSummary => {}
        JobRunProjectionProfile::ListHistory => {
            value["pid"] = json!(run.pid);
        }
        JobRunProjectionProfile::FullDetailStoredSteps => {
            add_run_worker_fields(&mut value, run);
        }
        JobRunProjectionProfile::FullDetailSiblingSteps => {
            add_run_worker_fields(&mut value, run);
            value["drain_last_pass"] = run_drain_last_pass_to_json(evidence.state);
            if let Some(fields) = value.as_object_mut() {
                fields.remove("steps");
            }
        }
        JobRunProjectionProfile::FullDetailEmbeddedSteps => {
            add_run_worker_fields(&mut value, run);
            if let Some(steps) = evidence.display_steps {
                value["steps"] = display_steps_with_agent_execution_to_json(
                    steps,
                    evidence.state,
                    evidence.activity_evidence.unwrap_or_default(),
                );
                value["steps_source"] = json!(steps.source.as_str());
            }
            value["drain_last_pass"] = run_drain_last_pass_to_json(evidence.state);
            // [ORB-14117] What an `--approve-proposed` drain approved and held.
            value["drain_approvals"] = serde_json::to_value(
                evidence
                    .state
                    .and_then(|state| state.pull_drain.drain_approvals.as_ref()),
            )
            .unwrap_or(Value::Null);
            // Recovery evidence stays separate from run/step errors: an
            // attempt never rewrites the failure that triggered it.
            // Unreadable trails stay explicit `unavailable`.
            value["recovery_attempts"] = match evidence.recovery {
                Some(attempts) => attempts.to_json(),
                None => RunRecoveryAttempts::unavailable().to_json(),
            };
        }
    }
    value
}

/// Drop the raw answer duplicates a full tool-show run carries: the parsed
/// answer already holds the useful fields, while `final_message` repeats
/// the provider envelope and `preview` its head. Durable blob refs stay.
pub fn trim_full_show_agent_invocation_answer(value: &mut Value) {
    if let Some(invocation) = value["agent_invocation"].as_object_mut()
        && invocation.get("answer").is_some_and(Value::is_object)
    {
        invocation.remove("preview");
        invocation.remove("preview_truncated");
        if let Some(answer) = invocation.get_mut("answer").and_then(Value::as_object_mut) {
            answer.remove("final_message");
        }
    }
}

/// What a run is doing right now: active step and provider processes.
/// Shared by the full tool show and the bounded read [ORB-11752].
pub fn run_execution_progress_to_json(progress: &RunExecutionProgress) -> Value {
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

/// Byte cap on a bounded read's agent answer.
pub const BOUNDED_RUN_ANSWER_MAX_BYTES: usize = 4096;
/// Step cap on a bounded read's displayed steps.
pub const BOUNDED_RUN_STEP_LIMIT: usize = 50;

/// Render a bounded read's displayed steps onto its run summary: capped
/// steps, `steps_source`, `steps_total` (null when the trail is
/// incomplete) and `steps_truncated`. Provider responses and original
/// inputs stay absent; error messages are bounded and redacted.
pub fn add_bounded_display_steps_to_json(
    value: &mut Value,
    steps: &RunDisplaySteps,
    state: Option<&PipelineState>,
    steps_complete: bool,
) {
    value["steps"] = json!(
        steps
            .records
            .iter()
            .take(BOUNDED_RUN_STEP_LIMIT)
            .map(|step| {
                let output = step.command_output_summary(state);
                let exit_code = step
                    .exit_code
                    .map(|code| json!(code))
                    .or_else(|| output.as_ref().map(|output| output["exit_code"].clone()));
                let mut step_value = json!({
                    "step_index": step.step_index, "target_id": step.target_id,
                    "target_type": step.target_type, "state": step.state,
                    "started_at": step.started_at, "finished_at": step.finished_at,
                    "duration_ms": step.duration_ms, "exit_code": exit_code,
                    "error_code": step.error_code,
                    "error_message": step.error_message.as_deref().map(|message| bounded_redacted_text(message, 4096).0),
                });
                if let Some(output) = output {
                    step_value["output"] = output;
                }
                step_value
            })
            .collect::<Vec<_>>()
    );
    value["steps_source"] = json!(steps.source.as_str());
    value["steps_total"] = json!(steps_complete.then_some(steps.records.len()));
    value["steps_truncated"] =
        json!(!steps_complete || steps.records.len() > BOUNDED_RUN_STEP_LIMIT);
}

/// Bound a bounded read's agent answer to [`BOUNDED_RUN_ANSWER_MAX_BYTES`]:
/// summary first, then whole fields that still fit; dropped fields are
/// named and `answer_retrieval` (built by the caller from its own routing
/// input) points at the full read. Persisted/full-view answers stay intact.
pub fn bound_bounded_agent_invocation_answer(
    invocation: &mut Value,
    answer_retrieval: Value,
) -> Result<(), OrbitError> {
    let Some(invocation) = invocation.as_object_mut() else {
        return Ok(());
    };
    let Some(answer) = invocation.get_mut("answer").and_then(Value::as_object_mut) else {
        return Ok(());
    };
    answer.remove("final_message");
    let mut original = std::mem::take(answer);
    let mut omitted = Vec::new();
    let mut truncated = false;
    let summary = original.remove("summary");
    for (key, mut field) in summary
        .map(|value| ("summary".to_string(), value))
        .into_iter()
        .chain(original)
    {
        if key == "summary"
            && let Some(text) = field.as_str()
        {
            // {"summary": ...} costs 12 bytes beside the JSON string.
            let prefix = json_string_prefix(text, BOUNDED_RUN_ANSWER_MAX_BYTES - 12);
            truncated |= prefix.len() < text.len();
            field = json!(prefix);
        }
        answer.insert(key.clone(), field);
        let bytes = serde_json::to_vec(answer)
            .map_err(|error| OrbitError::Execution(format!("compact agent answer: {error}")))?;
        if bytes.len() > BOUNDED_RUN_ANSWER_MAX_BYTES {
            answer.remove(&key);
            omitted.push(key);
            truncated = true;
        }
    }
    let summary = answer.get("summary").cloned().unwrap_or(Value::Null);
    invocation.remove("preview");
    invocation.remove("preview_truncated");
    invocation.insert("summary".into(), summary);
    invocation.insert(
        "answer_max_bytes".into(),
        json!(BOUNDED_RUN_ANSWER_MAX_BYTES),
    );
    invocation.insert("answer_truncated".into(), json!(truncated));
    invocation.insert("answer_omitted_fields".into(), json!(omitted));
    invocation.insert("answer_retrieval".into(), answer_retrieval);
    Ok(())
}

/// Prefix measured as a JSON string, counting quotes and escapes.
fn json_string_prefix(text: &str, max_bytes: usize) -> &str {
    let mut remaining = max_bytes.saturating_sub(2);
    let mut end = 0;
    for (index, ch) in text.char_indices() {
        let bytes = match ch {
            '"' | '\\' | '\n' | '\r' | '\t' | '\u{0008}' | '\u{000c}' => 2,
            ch if ch <= '\u{001f}' => 6,
            ch => ch.len_utf8(),
        };
        if bytes > remaining {
            break;
        }
        remaining -= bytes;
        end = index + ch.len_utf8();
    }
    &text[..end]
}
