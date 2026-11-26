use super::read::{resolve_run, resolve_step_filter};
use super::{Command, Operation, RunRead};
use crate::application::job::PipelineWorkerLogSnapshot;
use crate::runtime::audit::run::{RunAuditEvent, RunCliInvocationRecord};
use crate::runtime::audit::run_events::RunEventsSelection;
use crate::{JobRun, OrbitError, OrbitRuntime};
use std::collections::HashSet;

impl<'a> Command<'a, (JobRun, Vec<RunAuditEvent>)> {
    /// Read the events matching a run and optional step.
    pub fn events(
        run_id: Option<&'a str>,
        step_id: Option<&'a str>,
        event_type: Option<&'a str>,
        read: RunRead,
    ) -> Self {
        Self::new(
            Operation::command("run", "events").target(Some("job_run"), run_id.map(str::to_owned)),
            move |core| {
                let runtime = core.runtime();
                let run = resolve_run(runtime, run_id, read)?;
                let audit_steps = runtime.collect_run_audit_steps(&run.run_id)?;
                let step_filter = resolve_step_filter(&run, &audit_steps, step_id)?;
                let events = runtime.run_events_trail(
                    &run,
                    RunEventsSelection {
                        step_id: step_filter.as_deref(),
                        event_type,
                    },
                )?;

                Ok((run, events))
            },
        )
    }
}

impl<'a> Command<'a, (JobRun, Vec<RunAuditEvent>)> {
    /// Read the full audit trail of a run.
    pub fn trace(run_id: Option<&'a str>) -> Self {
        Self::new(
            Operation::command("run", "trace").target(Some("job_run"), run_id.map(str::to_owned)),
            move |core| {
                let rt = core.runtime();
                let run = resolve_run(rt, run_id, RunRead::Reconcile)?;
                let events = rt.collect_run_audit_events(&run.run_id)?;
                Ok((run, events))
            },
        )
    }
}

pub struct LogSnapshot {
    pub run: JobRun,
    pub records: Vec<RunCliInvocationRecord>,
    pub worker: Option<PipelineWorkerLogSnapshot>,
}

/// A subscription bound to the single run and step admitted at creation.
pub struct LogFollower {
    runtime: OrbitRuntime,
    run_id: String,
    step: Option<String>,
    read: RunRead,
}

impl LogFollower {
    pub fn run_id(&self) -> &str {
        &self.run_id
    }
    pub fn step_filtered(&self) -> bool {
        self.step.is_some()
    }
    pub fn state(&self) -> Result<JobRun, OrbitError> {
        self.read.show(&self.runtime, &self.run_id)
    }

    pub fn captures(
        &self,
        completed: &HashSet<String>,
    ) -> Result<Vec<RunCliInvocationRecord>, OrbitError> {
        self.runtime
            .collect_new_run_cli_invocations(&self.run_id, self.step.as_deref(), completed)
    }

    pub fn worker_log(&self) -> Result<Option<PipelineWorkerLogSnapshot>, OrbitError> {
        self.runtime.read_pipeline_worker_log(&self.run_id)
    }
}

impl<'a> Command<'a, LogSnapshot> {
    /// Load captured invocations and the worker fallback.
    pub fn logs(id: Option<&'a str>, step: Option<&'a str>, read: RunRead) -> Self {
        Self::new(
            Operation::command("run", "logs").target(Some("job_run"), id.map(str::to_owned)),
            move |core| {
                let rt = core.runtime();
                let run = resolve_run(rt, id, read)?;
                let steps = rt.collect_run_audit_steps(&run.run_id)?;
                let step = resolve_step_filter(&run, &steps, step)?;
                let records = rt.collect_new_run_cli_invocations(
                    &run.run_id,
                    step.as_deref(),
                    &HashSet::new(),
                )?;
                let worker = if records.is_empty() {
                    rt.read_pipeline_worker_log(&run.run_id)?
                } else {
                    None
                };
                Ok(LogSnapshot {
                    run,
                    records,
                    worker,
                })
            },
        )
    }
}

impl<'a> Command<'a, LogFollower> {
    /// Open a run-scoped log subscription.
    pub fn follow_logs(id: Option<&'a str>, step: Option<&'a str>, read: RunRead) -> Self {
        Self::new(
            Operation::command("run", "logs").target(Some("job_run"), id.map(str::to_owned)),
            move |core| {
                let rt = core.runtime();
                let run = resolve_run(rt, id, read)?;
                let steps = rt.collect_run_audit_steps(&run.run_id)?;
                let step = resolve_step_filter(&run, &steps, step)?;
                Ok(LogFollower {
                    runtime: rt.clone(),
                    run_id: run.run_id,
                    step,
                    read,
                })
            },
        )
    }
}
