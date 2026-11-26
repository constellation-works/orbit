//! Rows of the dashboard's run lists.

use super::lineage::add_run_lineage;
use super::task_labels::{add_tasks, task_titles};
use super::{Command, Operation};
use crate::application::job::{
    JOB_FAILURE_STREAK_WARN, JobCatalogEntry, JobCatalogFilter, JobFailureStreak, JobRunListParams,
};
use crate::application::job::{JobRunProjectionEvidence, JobRunProjectionProfile, project_job_run};
use crate::{JobRun, OrbitError, OrbitRuntime, PipelineInvokeResult};
use orbit_common::governance::authorization::{
    AuthorizationDenial, DASHBOARD_AUTO_DRAIN_COMPLETE, DASHBOARD_JOB_RUN,
};
use serde_json::Value;
use std::collections::BTreeMap;

pub struct JobRunsPage {
    pub items: Vec<Value>,
    pub total: u64,
    pub running: Option<Vec<Value>>,
}

/// Catalog entries with their latest runs and consecutive failures.
pub struct JobCatalog {
    pub rows: Vec<(JobCatalogEntry, Option<JobRun>)>,
    pub streaks: Vec<JobFailureStreak>,
}

impl Command<'static, JobCatalog> {
    /// Read the catalog with each job’s failure streak.
    pub fn catalog_with_streaks() -> Self {
        Self::new(
            Operation::command("job", "catalog-with-streaks"),
            move |core| {
                let runtime = core.runtime();
                let rows = runtime.list_job_catalog_with_last_run(true, JobCatalogFilter::All)?;
                let job_ids: Vec<String> =
                    rows.iter().map(|(entry, _)| entry.job_id.clone()).collect();
                let streaks = runtime.job_failure_streaks(&job_ids)?;
                Ok(JobCatalog { rows, streaks })
            },
        )
    }
}

impl<'a> Command<'a, (u32, Vec<JobFailureStreak>)> {
    /// Read failure streaks for manually runnable jobs.
    pub fn failure_streaks() -> Self {
        Self::new(Operation::command("job", "failure-streaks"), move |core| {
            let runtime = core.runtime();
            let threshold = JOB_FAILURE_STREAK_WARN;
            let job_ids: Vec<String> = runtime
                .list_job_catalog_with_last_run(true, JobCatalogFilter::All)?
                .into_iter()
                .filter(|(entry, _)| entry.supports_no_input_submission())
                .map(|(entry, _)| entry.job_id)
                .collect();
            let failing = runtime
                .job_failure_streaks(&job_ids)?
                .into_iter()
                .filter(|streak| streak.count >= threshold as usize)
                .collect::<Vec<_>>();
            Ok((threshold, failing))
        })
    }
}

impl<'a> Command<'a, JobRunsPage> {
    /// Read a bounded run page and its optional running companion after one reconciliation.
    pub fn job_page(params: JobRunListParams, running: Option<JobRunListParams>) -> Self {
        Self::new(Operation::command("job", "job-page"), move |core| {
            let rt = core.runtime();
            let runs = rt.list_job_runs(params.clone())?;
            let total = rt.count_job_runs_observed(params)?;
            let items = run_list_rows(rt, &runs, &task_titles(rt, &runs)?);
            let running = running
                .map(|params| -> Result<_, OrbitError> {
                    let runs = rt.list_job_runs_observed(params)?;
                    Ok(run_list_rows(rt, &runs, &task_titles(rt, &runs)?))
                })
                .transpose()?;
            Ok(JobRunsPage {
                items,
                total,
                running,
            })
        })
    }
}

/// Workspace-specific projection of the runs that survive a fleet-wide merge.
pub struct FleetProjection {
    runtime: OrbitRuntime,
    titles: BTreeMap<String, String>,
}

impl FleetProjection {
    pub fn rows(&self, runs: &[JobRun]) -> Vec<Value> {
        run_list_rows(&self.runtime, runs, &self.titles)
    }
}

pub struct FleetCandidates {
    pub runs: Vec<JobRun>,
    pub total: u64,
    pub projection: FleetProjection,
}

impl<'a> Command<'a, Vec<Result<FleetCandidates, OrbitError>>> {
    /// Collect the bounded candidate pages before the fleet merge.
    pub fn fleet_candidates(params: JobRunListParams, others: &'a [&'a crate::Core]) -> Self {
        Self::new(
            Operation::command("job", "fleet-candidates"),
            move |first| {
                Ok(super::fleet::read(first, others, |rt| {
                    let runs = rt.list_job_runs_observed(params.clone())?;
                    let total = rt.count_job_runs_observed(JobRunListParams {
                        limit: None,
                        ..params.clone()
                    })?;
                    let titles = task_titles(rt, &runs)?;
                    Ok(FleetCandidates {
                        runs,
                        total,
                        projection: FleetProjection {
                            runtime: rt.clone(),
                            titles,
                        },
                    })
                }))
            },
        )
    }
}

impl<'a> Command<'a, Result<PipelineInvokeResult, DashboardResumeError>> {
    /// Resume a run with its captured completion authority.
    pub fn resume_dashboard(id: &'a str, claim: Option<&'a str>, operator: bool) -> Self {
        let completion = super::exec::dashboard_authorize(&DASHBOARD_AUTO_DRAIN_COMPLETE, operator);
        Self::new(Operation::command("job", "resume-dashboard"), move |core| {
            let rt = core.runtime();
            let source = rt.show_job_run(id)?;
            if source
                .input
                .as_ref()
                .and_then(|input| input.get("completion"))
                .and_then(Value::as_str)
                == Some("done")
                && let Err(denial) = completion
            {
                return Ok(Err(DashboardResumeError::Denied(denial)));
            }
            Ok(rt
                .submit_resume_run(id, Some("dashboard"), claim)
                .map_err(DashboardResumeError::Submit))
        })
    }
}

pub enum DashboardResumeError {
    Denied(AuthorizationDenial),
    Submit(OrbitError),
}

impl<'a> Command<'a, Result<PipelineInvokeResult, DashboardJobError>> {
    /// Admit the dashboard session before reporting invalid input or submitting a job.
    pub fn submit_catalog_job(id: Result<&'a str, String>, operator: bool) -> Self {
        let admission = super::exec::dashboard_authorize(&DASHBOARD_JOB_RUN, operator);
        Self::new(
            Operation::command("job", "submit-catalog-job"),
            move |core| {
                if let Err(denial) = admission {
                    return Ok(Err(DashboardJobError::Denied(denial)));
                }
                let id = match id {
                    Ok(id) => id,
                    Err(message) => return Ok(Err(DashboardJobError::InvalidId(message))),
                };
                Ok(Ok(core.runtime().submit_no_input_catalog_job_run(
                    id,
                    Some("dashboard"),
                    orbit_types::workflow::JobRunTrigger::dashboard(),
                )?))
            },
        )
    }
}

pub enum DashboardJobError {
    Denied(AuthorizationDenial),
    InvalidId(String),
}

/// Project a run-list page: each run with its pipeline state, its tasks and
/// its dispatch lineage.
///
/// History names a cascaded failure's leaf from the shared row's
/// `root_cause` and groups a task's runs per attempt from `parent_run_id`.
/// When the states cannot be read, the rows keep their stateless shape
/// without `parent_run_id`, so History falls back to one group per task.
fn run_list_rows(
    runtime: &OrbitRuntime,
    runs: &[JobRun],
    titles: &BTreeMap<String, String>,
) -> Vec<Value> {
    let states = runtime
        .read_job_run_list_states(runs)
        .inspect_err(|error| {
            tracing::warn!(%error, "run list omitted pipeline state and dispatch lineage");
        })
        .ok();

    let mut values = runs
        .iter()
        .map(|run| {
            let mut value = match &states {
                Some(states) => states.row(run),
                None => project_job_run(
                    run,
                    JobRunProjectionProfile::ListRow,
                    JobRunProjectionEvidence::default(),
                ),
            };
            add_tasks(&mut value, run, titles);
            value
        })
        .collect::<Vec<_>>();

    if let Some(states) = &states {
        add_run_lineage(runs, states, &mut values);
    }
    values
}
