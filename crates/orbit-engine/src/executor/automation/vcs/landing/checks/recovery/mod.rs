//! Classifying failed hosted checks and recovering from them once.
//!
//! [`classify`] reads and classifies the failed jobs on the pull request's
//! head, [`act`] plans the rerun and routes the deciding class, and
//! [`dependency`] folds authenticated dependency waiters into the jobs they
//! name.

mod act;
mod classify;
mod dependency;

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use super::super::super::pr::{CheckState, PullRequestStatus};

pub(in crate::executor::automation::vcs) use act::recover_failed_checks;
pub(super) use dependency::unreadable_job_text;

/// The class of one failed hosted check, ordered so the class that decides
/// for several jobs is the greatest. [`Unknown`] records a log that could
/// not be read. It never outranks a readable job, and a set of only unknown
/// logs spends nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum CheckFailureClass {
    Unknown,
    Transient,
    Inherited,
    Flaky,
    Candidate,
    BaseSuspect,
    Inconclusive,
}

impl CheckFailureClass {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Transient => "transient",
            Self::Inherited => "inherited",
            Self::Flaky => "flaky",
            Self::Candidate => "candidate",
            Self::BaseSuspect => "base-suspect",
            Self::Inconclusive => "inconclusive",
        }
    }
}

/// What this landing attempt has already done about failed checks.
#[derive(Default)]
pub(in crate::executor::automation::vcs) struct CheckRecovery {
    rerun: bool,
    pub(in crate::executor::automation::vcs) rerun_diagnostic:
        Option<super::rerun_diagnostic::CiRerunDiagnostic>,
    /// Untouched failures whose same-check base pass authorized a rerun.
    /// The rerun's later completion must not demand yet another base pass.
    /// Repeated base-evidence observations keyed by unchanged head, check and signature.
    base_suspect_cycles: BTreeMap<(String, String, String), u64>,
    /// [ORB-21586] Consecutive polls on one head whose CI retry evidence
    /// could not be read or authenticated. Bounded by `max_base_ci_cycles`,
    /// so an unreadable retry escalates within minutes, not at the deadline.
    unreadable_rerun_cycles: BTreeMap<String, u64>,
    updated: bool,
    pub(in crate::executor::automation::vcs) max_base_ci_cycles: u64,
    pub(in crate::executor::automation::vcs) waiting_for_base: Option<String>,
    pub(super) deadline_ms: u64,
    /// Failed jobs already acted on, so a rollup that still shows them
    /// before the provider registers the rerun or update waits instead.
    handled: BTreeSet<u64>,
    /// Rollup entries those jobs answered, retained while replacement jobs
    /// have not registered yet. Separately failed checks still need evidence.
    handled_checks: BTreeSet<(Option<String>, String)>,
    /// Waits this attempt took for rate-limited check reads.
    pub(in crate::executor::automation::vcs) throttled_read_waits: u64,
    /// The classification each action was taken on, for the evidence.
    actions: Vec<String>,
    /// Dependency ids authenticated for a waiter job in this attempt, or
    /// `None` when its record did not authenticate: one adapter read per
    /// waiter, however often the attempt polls.
    dependency_auth: BTreeMap<(u64, u64), Option<Vec<u64>>>,
    /// Latest required-check read during failure classification; diagnostics only.
    pub(in crate::executor::automation::vcs) required_observation: Option<Value>,
    /// [model ranking §5.1] The task's recorded failures by cause.
    pub(in crate::executor::automation::vcs) failures: super::failure_ledger::FailureLedger,
}

impl CheckRecovery {
    pub(in crate::executor::automation::vcs) fn before(
        deadline_ms: u64,
        max_base_ci_cycles: u64,
    ) -> Self {
        Self {
            deadline_ms,
            max_base_ci_cycles,
            ..Default::default()
        }
    }
    /// The actions this attempt took, for the landing's completion evidence.
    pub(in crate::executor::automation::vcs) fn evidence(&self) -> Option<Value> {
        (!self.actions.is_empty()).then(|| json!(self.actions))
    }
}

/// What the landing does next about its failed checks.
pub(in crate::executor::automation::vcs) enum CheckRecoveryStep {
    /// The failed jobs were rerun or the branch updated; read the pull
    /// request again.
    Acted,
    /// Every failed job was already acted on; keep waiting.
    Wait,
    /// A base-suspect failure waits without authorizing candidate recovery.
    WaitForBase(String),
    /// A base-suspect failure spent its observation budget with the base's
    /// own result still missing: hold in review for that result, with this
    /// evidence. Nothing is repaired or rerun, so it is a designed wait, not a
    /// failure.
    HoldForBase(String),
    /// Stop for the one automatic repair, with this evidence.
    Repair(String),
    /// [ORB-16890] That repair is spent: stop so the owner returns the task
    /// to the backlog for its next run to repair the candidate, with this
    /// evidence. The landing run is held, not failed.
    Requeue(String),
    /// Stop in review and fail the run, with this evidence. A status that
    /// reports no head or number has no jobs to read, so it fails here too. A
    /// designed stay on a readable head — unclassified, inconclusive, or a
    /// transient failure the rerun planner will not run again — is
    /// [`Self::HoldInReview`].
    Stop(String),
    /// The candidate stays in review and the run is held, not failed.
    /// Unclassified evidence, inconclusive evidence, and a transient failure
    /// that already used its one rerun are designed stays: nothing is
    /// repaired, rerun, or updated.
    HoldInReview(String),
    /// The provider rate-limited a job or log read: wait for its reset and
    /// read again, without classifying around the missing evidence.
    ReadDeferred(super::read_wait::ThrottledRead),
}

/// The names of the checks the rollup reports failed, with their workflow.
pub(in crate::executor::automation::vcs) fn failed_checks(
    status: &PullRequestStatus,
) -> Vec<(Option<String>, String)> {
    status
        .status_checks
        .iter()
        .flatten()
        .filter(|check| {
            check.state == Some(CheckState::Failed) || check.owed_state == Some(CheckState::Failed)
        })
        .filter_map(|check| Some((check.workflow_name.clone(), check.name.clone()?)))
        .collect()
}

/// One failed job, or one failed required check run outside a workflow, on
/// the pull request's head.
pub(in crate::executor::automation::vcs) struct FailedJob {
    /// The workflow run to rerun; none for a check run outside a workflow.
    pub(super) run_id: Option<u64>,
    /// The job's, or the check run's, provider identity.
    pub(super) job_id: u64,
    pub(super) label: String,
    pub(super) class: CheckFailureClass,
    pub(super) signature: String,
    pub(super) why: String,
    pub(super) excerpt: String,
    /// The moved base tip's same job succeeded. A failure that remains on a
    /// head containing that tip is then the candidate's, not the base's.
    pub(super) tip_passes: bool,
}
