use super::leaves::{DrainLeafSummary, held_review_leaf, summarize_drain_leaves};
use super::lock_holders::{LockHolders, waiting_lock_holders};
use super::read::{resolve_run, resolve_run_step};
use super::{Command, Operation, RunRead};
use crate::application::job::{
    ActivityInvocationEvidence, RunDisplayStep, RunDisplaySteps, RunFailureCause,
    RunFailureContextProfile, RunFailureRead, job_run_task_ids, pull_drain_detail,
    run_display_steps,
};
use crate::runtime::audit::run::{RunProviderProcess, RunRecoveryAttempts, RunScriptExecution};
use crate::runtime::audit::tag_instructions::RunTagInstructions;
use crate::{CatalogReferenceLayer, JobRun, OrbitError, OrbitRuntime};
use orbit_types::task::TaskStatus;
use orbit_types::workflow::activity_job::{
    ACTIVITY_REF_PREFIX, ActivityV2Spec, JobV2Step, JobV2StepBody,
};
use orbit_types::workflow::{JobRunState, PipelineState};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashSet};

pub enum ShownRun {
    Step(Box<(JobRun, RunDisplayStep, Option<Value>)>),
    Full(Box<RunDetail>),
}

pub struct RunDetail {
    pub run: JobRun,
    pub state: Option<PipelineState>,
    pub checkpoint_unreadable: bool,
    pub provider_processes: Vec<RunProviderProcess>,
    pub script_executions: Vec<RunScriptExecution>,
    pub recovery: RunRecoveryAttempts,
    pub tag_instructions: RunTagInstructions,
    pub display_steps: RunDisplaySteps,
    pub root_causes: Vec<RunFailureCause>,
    pub lock_holders: LockHolders,
    pub drain_summary: Option<DrainLeafSummary>,
    pub catalog_layers: Vec<CatalogReferenceLayer>,
    pub pull: pull_drain_detail::RunPullDrainEvidence,
    pub effort_evidence: Vec<ActivityInvocationEvidence>,
    pub auto_concurrency: Option<Value>,
    pub task_titles: BTreeMap<String, String>,
    pub resumable_leaf: Option<String>,
    pub agent_activity_ids: HashSet<String>,
    pub waiting_tasks: WaitingTasks,
    pub worker_limit: Option<orbit_store::contract::WorkerResourceLimitEvidence>,
    pub url: Option<String>,
}

#[derive(Default)]
pub struct WaitingTasks {
    pub statuses: BTreeMap<String, TaskStatus>,
    pub host_fleet: BTreeMap<String, String>,
}

impl WaitingTasks {
    pub(super) fn read<'a>(rt: &OrbitRuntime, task_ids: impl Iterator<Item = &'a str>) -> Self {
        let statuses = rt.task_status_index().unwrap_or_else(|error| {
            tracing::warn!(%error, "waiting task statuses unavailable");
            Default::default()
        });
        let host_fleet = task_ids
            .filter_map(|id| {
                let task = rt.get_task(id).ok()?;
                Some((id.to_string(), rt.task_host_fleet_note(&task)?))
            })
            .collect();
        Self {
            statuses,
            host_fleet,
        }
    }
}

impl<'a> Command<'a, ShownRun> {
    /// Read a run detail or its single selected step.
    pub fn show(
        run_id: Option<&'a str>,
        step_id: Option<&'a str>,
        read: RunRead,
        mut warn: impl FnMut(String) + 'a,
    ) -> Self {
        Self::new(
            Operation::command("run", "show").target(Some("job_run"), run_id.map(str::to_owned)),
            move |core| inspect_run(core.runtime(), run_id, step_id, read, &mut warn),
        )
    }
}

fn inspect_run(
    runtime: &OrbitRuntime,
    run_id: Option<&str>,
    step_id: Option<&str>,
    read: RunRead,
    warn: &mut impl FnMut(String),
) -> Result<ShownRun, OrbitError> {
    let run = resolve_run(runtime, run_id, read)?;
    // An unreadable checkpoint is one section, as on HTTP and MCP. The run
    // record, steps, and provider processes stay readable; failing the whole
    // command hid them behind a store error.
    let (state, checkpoint_unreadable) = match runtime.read_run_state(&run.run_id) {
        Ok(state) => (state, false),
        Err(error) => {
            tracing::warn!(
                target: "orbit.cli.run",
                run_id = %run.run_id,
                %error,
                "pipeline checkpoint unreadable; run shown without it"
            );
            warn(format!(
                "warning: pipeline checkpoint unreadable for {}; run shown without it: {error}",
                run.run_id
            ));
            (None, true)
        }
    };

    if let Some(step_id) = step_id {
        let step = resolve_run_step(runtime, &run, step_id)?;
        let step_output = state.as_ref().and_then(|state| step.output(state)).cloned();
        return Ok(ShownRun::Step(Box::new((run, step, step_output))));
    }

    // [ORB-10496] Provider subprocesses spawned by this run's agent steps. A
    // ship-pipeline implementation agent is a child of the pipeline worker, not
    // of the Worker daemon, so this is the only place it is observable. The
    // same scan carries the step history a pipeline run keeps nowhere else
    // [ORB-12113].
    let audit = runtime.collect_run_audit_view(&run.run_id)?;
    let provider_processes = audit.provider_processes;
    let script_executions = audit.script_executions;
    let recovery = runtime.collect_run_recovery_attempts(&run.run_id)
            .unwrap_or_else(|error| {
                tracing::warn!(target: "orbit.cli.run", run_id = %run.run_id, %error, "recovery attempts unreadable");
                crate::runtime::audit::run::RunRecoveryAttempts::unavailable()
            });

    let tag_instructions = runtime
            .collect_run_tag_instructions(&run.run_id)
            .unwrap_or_else(|error| {
                tracing::warn!(target: "orbit.cli.run", run_id = %run.run_id, %error, "tag instructions unreadable");
                crate::runtime::audit::tag_instructions::RunTagInstructions::unavailable()
            });

    let display_steps = run_display_steps(&run, || audit.steps);
    let root_causes = collect_failed_leaf_causes(runtime, &run, state.as_ref(), read, warn)?;
    // Who holds the locks a waiting run is blocked on, read live: the run
    // only persists the selectors.
    let lock_holders = waiting_lock_holders(runtime, &run, state.as_ref());
    let mut drain_summary = summarize_drain_leaves(
        &run,
        state.as_ref(),
        |child_run_id| read.show(runtime, child_run_id).ok(),
        |leaf| held_review_leaf(runtime, leaf),
    );
    if let Some(summary) = drain_summary.as_mut() {
        for leaf in &mut summary.failed_leaves {
            leaf.resume_run_id = leaf_resume_target(runtime, &leaf.run_id, read, warn);
        }
    }

    let catalog_layers = runtime
        .catalog_reference_layers(&run.job_id)
        .unwrap_or_default();
    let pull = pull_drain_detail::read_run_pull_drain_evidence(
        pull_drain_detail::RunPullDrainSource::Registered(runtime),
        &run.run_id,
        state.as_ref(),
    );
    for warning in &pull.warnings {
        tracing::warn!(target: "orbit.cli.run", run_id = %run.run_id,
                section = warning.section, error = %warning.error, "run detail omitted a section");
    }
    let effort_evidence = runtime
        .activity_invocation_evidence(&run.run_id)
        .unwrap_or_default();
    let auto_concurrency = (run.job_id == "workspace_auto_pipeline")
        .then(|| runtime.run_auto_concurrency(&run.run_id))
        .transpose()?;
    let mut ids: BTreeSet<_> = job_run_task_ids(&run).into_iter().collect();
    if let Some(state) = &state {
        let current = state
            .pull_drain
            .drain_last_pass
            .as_ref()
            .and_then(|pass| pass.admission_forecast.as_ref());
        for forecast in current
            .into_iter()
            .chain(state.pull_drain.drain_last_forecast.as_ref())
        {
            ids.extend(forecast.admitted.iter().map(|task| task.task_id.clone()));
            ids.extend(forecast.skipped.iter().map(|task| task.task_id.clone()));
            ids.extend(forecast.head.iter().map(|task| task.task_id.clone()));
        }
    }
    let task_titles = ids
        .into_iter()
        .filter_map(|id| runtime.get_task(&id).ok().map(|task| (id, task.title)))
        .collect();
    let resumable_leaf = resume_leaf(runtime, &run, &root_causes, read);
    let agent_activity_ids = agent_activity_ids_for_job(runtime, &run.job_id);
    let waiting_tasks = if let Some(summary) = &drain_summary {
        WaitingTasks::read(runtime, summary.host_os_waiting_task_ids())
    } else if state
        .as_ref()
        .is_some_and(|s| s.pull_drain.drain_last_pass.is_some())
    {
        WaitingTasks::read(runtime, std::iter::empty())
    } else {
        WaitingTasks::default()
    };
    let worker_limit = runtime.worker_resource_limit_evidence(&run.run_id)?;
    let url = runtime.run_permalink_url(&run);
    Ok(ShownRun::Full(Box::new(RunDetail {
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
    })))
}

/// Activity step ids whose workflow definition invokes an agent. This only
/// affects human rendering; `activity_provenance` stays complete in JSON.
fn agent_activity_ids_for_job(runtime: &OrbitRuntime, job_id: &str) -> HashSet<String> {
    let job = match runtime.show_job_catalog_entry(job_id) {
        Ok(entry) => entry.spec,
        Err(error) => {
            tracing::debug!(%job_id, %error, "run show cannot resolve activity provenance definitions");
            return HashSet::new();
        }
    };
    let agent_activity_names = runtime
        .v2_activity_catalog()
        .ok()
        .map(|catalog| {
            catalog
                .names()
                .filter(|name| {
                    catalog.get(name).is_some_and(|activity| {
                        matches!(&activity.spec, ActivityV2Spec::AgentLoop(_))
                    })
                })
                .map(str::to_string)
                .collect::<HashSet<_>>()
        })
        .unwrap_or_default();
    let mut ids = HashSet::new();
    collect_agent_activity_ids(&job.steps, &agent_activity_names, &mut ids);
    ids
}

fn collect_agent_activity_ids(
    steps: &[JobV2Step],
    agent_activity_names: &HashSet<String>,
    ids: &mut HashSet<String>,
) {
    for step in steps {
        match &step.body {
            JobV2StepBody::Target(target) => {
                if matches!(&target.spec, ActivityV2Spec::AgentLoop(_)) {
                    ids.insert(step.id.clone());
                }
            }
            JobV2StepBody::TargetRef(target) => {
                if let Some(name) = target.target.strip_prefix(ACTIVITY_REF_PREFIX)
                    && agent_activity_names.contains(name)
                {
                    ids.insert(step.id.clone());
                }
            }
            JobV2StepBody::Parallel { parallel } => {
                collect_agent_activity_ids(&parallel.branches, agent_activity_names, ids);
            }
            JobV2StepBody::FanOut { fan_out, .. } => {
                collect_agent_activity_ids(
                    std::slice::from_ref(fan_out.worker.as_ref()),
                    agent_activity_names,
                    ids,
                );
            }
            JobV2StepBody::Loop { loop_ } => {
                collect_agent_activity_ids(&loop_.steps, agent_activity_names, ids);
            }
        }
    }
}

/// Where to resume a failed pipeline run whose failure came from a child run.
///
/// A coordinating run (a ship or drain wrapper) fails only because its child
/// did. Resuming the wrapper re-checks that same failed child result and fails
/// the same way, so the operator needs the run that did the work.
fn resume_leaf(
    runtime: &OrbitRuntime,
    run: &JobRun,
    causes: &[RunFailureCause],
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
    Some(leaf.run_id.clone())
}

/// The run to resume for a failed leaf: the same choice the CLI resume hint makes
/// for a run of its own, so the drain's advice and the leaf's agree.
fn leaf_resume_target(
    runtime: &OrbitRuntime,
    leaf_run_id: &str,
    read: RunRead,
    warn: &mut impl FnMut(String),
) -> Option<String> {
    let leaf = read.show(runtime, leaf_run_id).ok()?;
    let state = runtime.read_run_state(&leaf.run_id).ok().flatten();
    let causes = collect_failed_leaf_causes(runtime, &leaf, state.as_ref(), read, warn).ok()?;
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

/// `orbit run show`'s failed terminal leaves, through the shared failure
/// query. A wrapper's own error stays in its normal run header/steps.
fn collect_failed_leaf_causes(
    runtime: &OrbitRuntime,
    run: &JobRun,
    state: Option<&PipelineState>,
    read: RunRead,
    warn: &mut impl FnMut(String),
) -> Result<Vec<RunFailureCause>, OrbitError> {
    let read = match read {
        RunRead::Reconcile => RunFailureRead::Reconcile,
        RunRead::Observe => RunFailureRead::Observe,
    };
    let context = runtime.run_failure_context(
        run,
        state,
        RunFailureContextProfile::AllFailedLeaves(read),
        // This profile survives only an unreadable child checkpoint.
        &mut |warning| {
            tracing::warn!(
                target: "orbit.cli.run",
                run_id = %warning.run_id,
                error = %warning.error,
                "pipeline checkpoint unreadable; run shown without it"
            );
            warn(format!(
                "warning: pipeline checkpoint unreadable for {}; run shown without it: {}",
                warning.run_id, warning.error
            ));
        },
    )?;
    Ok(context.causes)
}
