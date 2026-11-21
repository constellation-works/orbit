use orbit_types::workflow::{
    FaultAttribution, FinalRecoveryDecision, FinalRecoveryResume, PipelineState,
};

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
    /// Applying the decision failed, or the host answered `resume` with no
    /// step to resume from; the run fails with its `failure_activity`, as an
    /// escalation does. Distinct from [`Self::Escalated`], which records a
    /// park the host successfully applied.
    RecoveryApplicationFailed,
    /// A park or current-policy requeue the host successfully applied for a
    /// verified unchanged completion observation [ORB-25661]; the executor
    /// holds the run instead of failing it, without its `failure_activity`.
    CompletionObservationHeld,
}

impl FinalRecoveryVerdict {
    /// Whether the job's terminal `failure_activity` still runs.
    pub(in crate::activity_job::job_executor) fn runs_failure_activity(&self) -> bool {
        matches!(
            self,
            Self::Skipped | Self::Escalated | Self::RecoveryApplicationFailed
        )
    }

    /// This verdict as the hold for a transient completion observation, when
    /// it records a disposition the host successfully applied. A failed
    /// application and every other verdict keep their own path.
    pub(in crate::activity_job::job_executor) fn held_for_completion_observation(self) -> Self {
        match self {
            Self::Settled | Self::Escalated => Self::CompletionObservationHeld,
            other => other,
        }
    }
}

/// Whether this run may still invoke its final recovery.
#[derive(Debug)]
pub(in crate::activity_job::job_executor) enum FinalRecoveryBudget {
    /// Not invoked yet.
    Available,
    /// A run this one resumed decided this and did not resume; the host
    /// applies it again, with its recorded fault, without a second dispatch.
    Replay(FinalRecoveryDecision, FaultAttribution),
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
                    Self::Replay(decision.clone(), checkpoint.fault.clone())
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
