use crate::application::job::{JobRunListParams, RunDisplayStep as RunStepRecord};
use crate::runtime::audit::run::RunAuditStep;
use crate::{JobRun, JobRunStep, OrbitError, OrbitRuntime};
use orbit_types::workflow::JobTargetType;

/// Whether a run read may reconcile stale runs before reporting them.
///
/// `Reconcile` is the operator default: an orphaned `pending`/`running` run
/// is finalized as `interrupted`, releasing its task reservations, so the
/// view shows what actually happened. It then settles with their owner the
/// pull claims an ended drain or leaf left that nothing else will carry
/// [ORB-15783]. `Observe` reports stored records
/// untouched, for readers that promise not to mutate run state [ORB-12941].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunRead {
    Reconcile,
    Observe,
}

impl RunRead {
    pub fn from_no_reconcile(no_reconcile: bool) -> Self {
        if no_reconcile {
            Self::Observe
        } else {
            Self::Reconcile
        }
    }

    pub(super) fn show(self, runtime: &OrbitRuntime, run_id: &str) -> Result<JobRun, OrbitError> {
        match self {
            Self::Reconcile => {
                let run = runtime.show_job_run(run_id)?;
                crate::unit::pull_drain::settle_orphaned_pulls(runtime);
                Ok(run)
            }
            Self::Observe => runtime.show_job_run_observed(run_id),
        }
    }

    pub(super) fn list(
        self,
        runtime: &OrbitRuntime,
        params: JobRunListParams,
    ) -> Result<Vec<JobRun>, OrbitError> {
        match self {
            Self::Reconcile => {
                let runs = runtime.list_job_runs(params)?;
                crate::unit::pull_drain::settle_orphaned_pulls(runtime);
                Ok(runs)
            }
            Self::Observe => runtime.list_job_runs_observed(params),
        }
    }
}

pub(super) fn resolve_run(
    runtime: &OrbitRuntime,
    run_id: Option<&str>,
    read: RunRead,
) -> Result<JobRun, OrbitError> {
    if let Some(run_id) = run_id {
        return read.show(runtime, run_id);
    }

    read.list(
        runtime,
        JobRunListParams {
            limit: Some(1),
            ..Default::default()
        },
    )?
    .into_iter()
    .next()
    .ok_or(OrbitError::NoJobRuns)
}

pub(super) fn resolve_run_step(
    runtime: &OrbitRuntime,
    run: &JobRun,
    step_id: &str,
) -> Result<RunStepRecord, OrbitError> {
    let audit_steps = runtime.collect_run_audit_steps(&run.run_id)?;
    if let Some(audit_step) = audit_steps.iter().find(|step| step.step_id == step_id) {
        return Ok(RunStepRecord::from_audit(audit_step.clone(), run.state));
    }

    let audit_index = step_id
        .parse::<u32>()
        .ok()
        .and_then(|index| audit_steps.iter().find(|step| step.step_index == index));
    let audit_is_displayed = !audit_steps.is_empty()
        && run
            .steps
            .iter()
            .all(|step| step.target_type == JobTargetType::Job);
    if audit_is_displayed && let Some(audit_step) = audit_index {
        return Ok(RunStepRecord::from_audit(audit_step.clone(), run.state));
    }

    if let Some(stored_step) = find_stored_run_step(run, step_id) {
        return Ok(RunStepRecord::from_job_step(stored_step));
    }
    if let Some(audit_step) = audit_index {
        return Ok(RunStepRecord::from_audit(audit_step.clone(), run.state));
    }

    Err(step_not_found(&run.run_id, step_id))
}

fn find_stored_run_step<'a>(run: &'a JobRun, step_id: &str) -> Option<&'a JobRunStep> {
    run.steps
        .iter()
        .find(|step| step.target_id == step_id || step.step_index.to_string() == step_id)
}

fn step_not_found(run_id: &str, step_id: &str) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "step '{step_id}' does not match any step in run '{run_id}'"
    ))
}

pub(super) fn resolve_step_filter(
    run: &JobRun,
    audit_steps: &[RunAuditStep],
    step_id: Option<&str>,
) -> Result<Option<String>, OrbitError> {
    let Some(step_id) = step_id else {
        return Ok(None);
    };

    if let Some(step) = audit_steps.iter().find(|step| step.step_id == step_id) {
        return Ok(Some(step.step_id.clone()));
    }
    if let Ok(index) = step_id.parse::<u32>()
        && let Some(step) = audit_steps.iter().find(|step| step.step_index == index)
    {
        return Ok(Some(step.step_id.clone()));
    }
    if find_stored_run_step(run, step_id).is_some() {
        return Ok(Some(step_id.to_string()));
    }

    Err(step_not_found(&run.run_id, step_id))
}
