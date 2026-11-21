use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use chrono::Utc;
use orbit_automation::routines::breaker::{ObservedFire, ObservedOutcome};
use orbit_common::OrbitError;
use orbit_store::contracts::JobRunQuery;
use orbit_types::workflow::{JobRun, JobRunState};
use serde_json::json;

use super::super::loader;
use super::{RoutineDispatch, RunOwnerLiveness};
use crate::OrbitRuntime;
use crate::application::automation::SourceCache;
use crate::application::job::run_owner_liveness;

/// Production dispatch over the per-workspace runtimes discovered this pass.
pub(super) struct RuntimeDispatch<'a> {
    pub(super) runtimes: BTreeMap<PathBuf, &'a OrbitRuntime>,
    pub(super) shown_runs: RefCell<HashMap<(PathBuf, String), JobRun>>,
    pub(super) source_cache: SourceCache,
}

impl RoutineDispatch for RuntimeDispatch<'_> {
    fn evaluate_delivery(
        &self,
        routine: &loader::LoadedRoutine,
        dry_run: bool,
        now: chrono::DateTime<Utc>,
    ) -> Result<orbit_types::workflow::automation::AutomationDiagnostic, OrbitError> {
        let runtime = self
            .runtimes
            .get(&routine.source_orbit_dir)
            .ok_or_else(|| OrbitError::Execution("routine source missing".into()))?;
        crate::application::automation::evaluate_routine_with_cache(
            runtime,
            &routine.definition,
            dry_run,
            now,
            Some(&self.source_cache),
        )
    }

    fn evaluate_condition(
        &self,
        routine: &loader::LoadedRoutine,
        dry_run: bool,
        now: chrono::DateTime<Utc>,
    ) -> Result<orbit_automation::condition::ConditionDiagnostic, OrbitError> {
        let runtime = self
            .runtimes
            .get(&routine.source_orbit_dir)
            .ok_or_else(|| OrbitError::Execution("routine source missing".into()))?;
        crate::application::automation::condition::evaluate_routine_condition(
            runtime,
            &routine.definition,
            dry_run,
            now,
        )
    }

    fn evaluate_guard(
        &self,
        routine: &loader::LoadedRoutine,
        now: chrono::DateTime<Utc>,
    ) -> orbit_automation::condition::GuardVerdict {
        let Some(when) = routine.definition.trigger.when.as_deref() else {
            return orbit_automation::condition::GuardVerdict::Open;
        };
        match self.runtimes.get(&routine.source_orbit_dir) {
            Some(runtime) => crate::application::automation::condition::evaluate_guard(
                runtime,
                when,
                "trigger.when",
                orbit_automation::condition::ConditionCarrier::Routine,
                now,
            ),
            None => orbit_automation::condition::GuardVerdict::unknown("routine source missing"),
        }
    }

    fn submit(
        &self,
        source_orbit_dir: &Path,
        job_name: &str,
        actor: &str,
        slot: &str,
    ) -> Result<String, OrbitError> {
        let runtime = self.runtimes.get(source_orbit_dir).ok_or_else(|| {
            OrbitError::WorkspaceError(format!(
                "no runtime for source workspace '{}'",
                source_orbit_dir.display()
            ))
        })?;
        // [ORB-11998] Carry the owning workspace's `.orbit` directory into the
        // run explicitly, so the detached worker that ends up executing it can
        // verify its own resolved workspace matches this one — instead of the
        // run silently trusting whatever the worker's cwd/env resolved to.
        let mut input = json!({});
        input[crate::application::job::pipeline::ROUTINE_DISPATCH_ORBIT_DIR_FIELD] =
            json!(source_orbit_dir.to_string_lossy());
        let routine = actor.strip_prefix("routine/").unwrap_or(actor);
        runtime
            .submit_pipeline_run_with_trigger(
                job_name,
                input,
                None,
                Some(actor),
                orbit_types::workflow::JobRunTrigger::routine(routine, slot),
            )
            .map(|invoke| invoke.run_id)
    }

    fn run_state(&self, source_orbit_dir: &Path, run_id: &str) -> Option<JobRunState> {
        self.shown_job_run(source_orbit_dir, run_id)
            .map(|run| run.state)
    }

    fn run_owner_liveness(&self, source_orbit_dir: &Path, run_id: &str) -> RunOwnerLiveness {
        self.shown_job_run(source_orbit_dir, run_id)
            // An unreadable run is not evidence that its worker stopped.
            .map_or(RunOwnerLiveness::Unknown, |run| {
                match run_owner_liveness(&run) {
                    crate::application::job::RunOwnerLiveness::Alive => RunOwnerLiveness::Alive,
                    crate::application::job::RunOwnerLiveness::Stopped => RunOwnerLiveness::Stopped,
                    crate::application::job::RunOwnerLiveness::Unknown => RunOwnerLiveness::Unknown,
                }
            })
    }

    fn routine_runs(
        &self,
        routine: &loader::LoadedRoutine,
        limit: usize,
    ) -> Result<Vec<ObservedFire>, OrbitError> {
        let Some(runtime) = self.runtimes.get(&routine.source_orbit_dir) else {
            return Ok(Vec::new());
        };
        let runs = runtime
            .stores()
            .jobs()
            .list_job_runs_filtered(&JobRunQuery {
                job_id: Some(routine.definition.target.job_name().to_string()),
                trigger_routine: Some(routine.definition.name.clone()),
                limit: Some(limit),
                include_steps: false,
                ..JobRunQuery::default()
            })?;
        Ok(runs
            .into_iter()
            .map(|run| ObservedFire {
                created_at: run.created_at,
                outcome: match run.state {
                    JobRunState::Pending | JobRunState::Running | JobRunState::Retrying => {
                        ObservedOutcome::InFlight
                    }
                    JobRunState::Success | JobRunState::Held => ObservedOutcome::Succeeded,
                    JobRunState::Skipped => ObservedOutcome::Ignored,
                    JobRunState::Failed
                    | JobRunState::Timeout
                    | JobRunState::Cancelled
                    | JobRunState::Interrupted => ObservedOutcome::Failed(None),
                },
                run_id: Some(run.run_id.to_string()),
            })
            .collect())
    }

    fn run_failure_class(&self, source_orbit_dir: &Path, run_id: &str) -> Option<String> {
        let run = self
            .runtimes
            .get(source_orbit_dir)?
            .stores()
            .jobs()
            .get_job_run(run_id)
            .ok()
            .flatten()?;
        let code = run
            .steps
            .iter()
            .filter(|step| step.state != JobRunState::Success)
            .find_map(|step| step.typed_failure_code())
            .map(|code| code.as_str().to_string());
        Some(code.unwrap_or_else(|| run.state.to_string()))
    }

    fn host_orbit_build(&self) -> Option<String> {
        let build = orbit_common::process::build::current_orbit_build();
        Some(match build.source_commit {
            Some(commit) => format!("{}+{commit}", build.version),
            None => build.version,
        })
    }
}

impl RuntimeDispatch<'_> {
    fn shown_job_run(&self, source_orbit_dir: &Path, run_id: &str) -> Option<JobRun> {
        let key = (source_orbit_dir.to_path_buf(), run_id.to_string());
        if let Some(run) = self.shown_runs.borrow().get(&key) {
            return Some(run.clone());
        }
        let run = self
            .runtimes
            .get(source_orbit_dir)
            .and_then(|runtime| runtime.show_job_run(run_id).ok())?;
        self.shown_runs.borrow_mut().insert(key, run.clone());
        Some(run)
    }
}
