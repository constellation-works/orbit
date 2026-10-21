//! Bounded walk over a run's durable `retry_source_run_id` ancestry.

use std::collections::BTreeSet;

use orbit_common::OrbitError;
use orbit_types::workflow::JobRun;

use crate::context::RuntimeHost;

/// The most retry ancestors one lineage walk loads.
pub(super) const RETRY_LINEAGE_MAX_HOPS: usize = 64;

/// One step of a [`RetryLineage`] walk.
pub(super) enum LineageStep {
    /// The next ancestor: the id its descendant names and the run stored
    /// under that id.
    Ancestor(String, Box<JobRun>),
    /// The walk ended; callers stop at the first stop.
    Stop(LineageStop),
}

/// Why a [`RetryLineage`] walk ended.
pub(super) enum LineageStop {
    /// The last run names no retry source.
    Root,
    /// No run is stored under the named retry source.
    Missing(String),
    /// The ancestor `run_id` belongs to `job_id`, not the starting run's job.
    CrossJob { run_id: String, job_id: String },
    /// The named retry source was already yielded by this walk.
    Cycle,
    /// The run stored under the named id records a different run id.
    IdentityMismatch,
    /// [`RETRY_LINEAGE_MAX_HOPS`] ancestors were yielded; the last one's
    /// retry source is not inspected.
    Bound,
}

/// Walks retry ancestors upward from a starting run, loading one stored run
/// per step. The starting run is neither yielded nor counted as seen, so a
/// lineage that loops back to it yields it once before reporting the cycle.
pub(super) struct RetryLineage {
    next: Option<String>,
    job_id: Option<String>,
    check_identity: bool,
    seen: BTreeSet<String>,
}

impl RetryLineage {
    /// Walk ancestors that must belong to `start`'s job.
    pub(super) fn same_job(start: &JobRun) -> Self {
        Self::new(start, Some(start.job_id.clone()))
    }

    /// Walk ancestors whatever job they belong to.
    pub(super) fn any_job(start: &JobRun) -> Self {
        Self::new(start, None)
    }

    fn new(start: &JobRun, job_id: Option<String>) -> Self {
        Self {
            next: start.retry_source_run_id.clone(),
            job_id,
            check_identity: false,
            seen: BTreeSet::new(),
        }
    }

    /// Also stop when a stored ancestor records a different run id than the
    /// one its descendant names.
    pub(super) fn checking_identity(mut self) -> Self {
        self.check_identity = true;
        self
    }

    /// Load the next ancestor, or report why the walk ended.
    pub(super) fn step<H: RuntimeHost + ?Sized>(
        &mut self,
        host: &H,
    ) -> Result<LineageStep, OrbitError> {
        if self.seen.len() == RETRY_LINEAGE_MAX_HOPS {
            return Ok(LineageStep::Stop(LineageStop::Bound));
        }
        let Some(run_id) = self.next.take() else {
            return Ok(LineageStep::Stop(LineageStop::Root));
        };
        if !self.seen.insert(run_id.clone()) {
            return Ok(LineageStep::Stop(LineageStop::Cycle));
        }
        let Some(run) = host.get_job_run(&run_id)? else {
            return Ok(LineageStep::Stop(LineageStop::Missing(run_id)));
        };
        if self.check_identity && run.run_id != run_id {
            return Ok(LineageStep::Stop(LineageStop::IdentityMismatch));
        }
        if self
            .job_id
            .as_ref()
            .is_some_and(|job_id| *job_id != run.job_id)
        {
            return Ok(LineageStep::Stop(LineageStop::CrossJob {
                run_id: run.run_id,
                job_id: run.job_id,
            }));
        }
        self.next.clone_from(&run.retry_source_run_id);
        Ok(LineageStep::Ancestor(run_id, Box::new(run)))
    }
}
