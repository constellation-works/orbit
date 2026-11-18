use std::collections::BTreeSet;

use chrono::{DateTime, SecondsFormat, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::JobRunQuery;
use orbit_types::workflow::{JobRun, JobRunState, PipelineState};

use crate::OrbitRuntime;
use crate::application::job::drain_deadline;

use super::super::drains::{ActiveDrain, DRAIN_JOB_NAME, LEAF_JOB_NAME};

/// The status drain's progress: the most recent finished drain when no local
/// drain is live, the status drain's pipeline state, its window deadline and
/// workers, and the phase readiness reports. Readiness and the dashboard's
/// change stream read the phase from here, so they never disagree.
pub(super) struct DrainProgress {
    pub(super) recent_drain: Option<JobRun>,
    pub(super) status_state: Option<PipelineState>,
    pub(super) ends_at: Option<String>,
    pub(super) admitted_workers: usize,
    pub(super) running_admitted_workers: usize,
    pub(super) phase: &'static str,
}

pub(super) fn drain_progress(
    runtime: &OrbitRuntime,
    active_drain: Option<&ActiveDrain>,
) -> Result<DrainProgress, OrbitError> {
    let recent_drain = if active_drain.is_none() {
        runtime
            .stores()
            .jobs()
            .list_job_runs_filtered(&JobRunQuery {
                job_id: Some(DRAIN_JOB_NAME.to_string()),
                terminal_only: true,
                limit: Some(1),
                include_steps: false,
                ..JobRunQuery::default()
            })?
            .into_iter()
            .next()
    } else {
        None
    };
    let status_run_id = active_drain
        .map(|drain| drain.run_id.as_str())
        .or_else(|| recent_drain.as_ref().map(|run| run.run_id.as_str()));
    let status_state = status_run_id
        .map(|run_id| runtime.stores().jobs().read_run_state(run_id))
        .transpose()?
        .flatten();
    // The open-window checkpoint is the actual server-stamped deadline, not
    // the submission time plus a browser's chosen duration, unless an
    // operator has since extended it [ORB-15383].
    let ends_at = status_state
        .as_ref()
        .and_then(drain_deadline)
        .map(|deadline| deadline.to_rfc3339_opts(SecondsFormat::Secs, true));
    let (admitted_workers, running_admitted_workers) = if let Some(state) = &status_state {
        let created_since = active_drain
            .map(|drain| drain.created_at)
            .or_else(|| recent_drain.as_ref().map(|run| run.created_at));
        let states = [
            JobRunState::Pending,
            JobRunState::Running,
            JobRunState::Success,
            JobRunState::Failed,
            JobRunState::Timeout,
            JobRunState::Skipped,
            JobRunState::Retrying,
            JobRunState::Cancelled,
            JobRunState::Interrupted,
            JobRunState::Held,
        ]
        .into_iter()
        .filter(|state| !state.is_terminal())
        .collect();
        let running_ids: BTreeSet<_> = runtime
            .stores()
            .jobs()
            .list_job_runs_filtered(&JobRunQuery {
                job_id: Some(LEAF_JOB_NAME.to_string()),
                states,
                created_since,
                include_steps: false,
                input_keys: Some(&[]),
                ..JobRunQuery::default()
            })?
            .into_iter()
            .map(|run| run.run_id)
            .collect();
        let workers = state
            .child_dispatches
            .iter()
            .filter(|dispatch| dispatch.job_name == LEAF_JOB_NAME)
            .collect::<Vec<_>>();
        let running = workers
            .iter()
            .filter(|dispatch| running_ids.contains(&dispatch.child_run_id))
            .count();
        (workers.len(), running)
    } else {
        (0, 0)
    };
    let window_open = ends_at
        .as_deref()
        .and_then(|deadline| DateTime::parse_from_rfc3339(deadline).ok())
        .is_none_or(|deadline| deadline > Utc::now());
    let phase = if active_drain.is_some_and(|drain| !drain.admissions_stopped()) && window_open {
        "draining"
    } else if running_admitted_workers > 0 {
        "winding_down"
    } else {
        "idle"
    };
    Ok(DrainProgress {
        recent_drain,
        status_state,
        ends_at,
        admitted_workers,
        running_admitted_workers,
        phase,
    })
}
