//! Why a published source is not an exact owner-local single-task execution.
//!
//! `job` decides it (source repair's identity check); `task` reads it when a
//! final-recovery requeue would preserve a publication the next run's
//! candidate resume then refuses.

/// The one owner-source identity predicate a published source failed, in the
/// order source repair checks them. Each refusal names its predicate, so an
/// operator can tell machine identity, input shape, pull and claim failures
/// apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OwnerSourceIdentityRefusal {
    /// This host has no automation machine identity to compare against.
    LocalIdentityMissing,
    /// The source is not a `task_pr_pipeline` delivery.
    JobKind,
    /// The source run records no executing machine, or another one.
    SourceMachine,
    /// The task's run binding names no machine, or another one.
    TaskMachine,
    /// The source ran a follower's claimed input.
    ClaimedInput,
    /// The source input is missing or does not carry exactly this task.
    TaskSet,
    /// The source run was admitted through a local pull.
    LocalPull,
    /// The task holds an execution claim.
    ExecutionClaim,
}

impl OwnerSourceIdentityRefusal {
    /// Stable code recorded with a refused requeue. Persisted: never reword.
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::LocalIdentityMissing => "local_identity_missing",
            Self::JobKind => "job_kind",
            Self::SourceMachine => "source_machine",
            Self::TaskMachine => "task_machine",
            Self::ClaimedInput => "claimed_input",
            Self::TaskSet => "task_set",
            Self::LocalPull => "local_pull",
            Self::ExecutionClaim => "execution_claim",
        }
    }

    /// What the failed predicate means, for refusal text.
    pub(crate) fn detail(self) -> &'static str {
        match self {
            Self::LocalIdentityMissing => "this host has no machine identity",
            Self::JobKind => "the source run is not a task PR pipeline delivery",
            Self::SourceMachine => "the source run did not execute on this machine",
            Self::TaskMachine => "the task's run binding names another machine",
            Self::ClaimedInput => "the source run executed a claimed follower input",
            Self::TaskSet => "the source run input does not carry exactly this task",
            Self::LocalPull => "the source run was admitted through a local pull",
            Self::ExecutionClaim => "the task holds an execution claim",
        }
    }
}

impl std::fmt::Display for OwnerSourceIdentityRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.code(), self.detail())
    }
}
