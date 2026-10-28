use orbit_types::workflow::{FinalRecoveryDecision, FinalRecoveryResume, PipelineState};

/// What the executor does after the hook.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::activity_job::job_executor) enum FinalRecoveryVerdict {
    /// The hook did not run; today's failure path follows.
    Skipped,
    /// Rerun the pipeline from this top-level step index.
    Resume(usize, Box<FinalRecoveryResume>),
    /// The task was settled; the run fails without its `failure_activity`.
    Settled,
    /// The task is parked for a human; the run's `failure_activity` follows.
    Escalated,
}

impl FinalRecoveryVerdict {
    /// Whether the job's terminal `failure_activity` still runs.
    pub(in crate::activity_job::job_executor) fn runs_failure_activity(&self) -> bool {
        matches!(self, Self::Skipped | Self::Escalated)
    }
}

/// Whether this run may still invoke its final recovery.
#[derive(Debug)]
pub(in crate::activity_job::job_executor) enum FinalRecoveryBudget {
    /// Not invoked yet.
    Available,
    /// A run this one resumed decided this and did not resume; the host
    /// applies it again, without a second dispatch.
    Replay(FinalRecoveryDecision),
    /// Invoked already, by this run or one it resumed.
    Spent,
}

impl FinalRecoveryBudget {
    pub(in crate::activity_job::job_executor) fn from_resume(
        resume: Option<&PipelineState>,
    ) -> Self {
        match resume.and_then(|state| state.final_recovery.as_ref()) {
            None => Self::Available,
            Some(checkpoint) => match &checkpoint.decision {
                Some(decision) if !matches!(decision, FinalRecoveryDecision::Resume { .. }) => {
                    Self::Replay(decision.clone())
                }
                _ => Self::Spent,
            },
        }
    }
}

/// The failed run's worktree, as its earliest step reported it.
pub(in crate::activity_job::job_executor) struct RunWorktree {
    pub(in crate::activity_job::job_executor) workspace_path: String,
    pub(super) base_ref: Option<String>,
    pub(in crate::activity_job::job_executor) base_sha: Option<String>,
}
