use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, Utc};
use orbit_types::task::{RegressionClassification, TaskComplexity, TaskOrigin, TaskStatus};
use orbit_types::workflow::ReviewVerdict;

use super::super::attribution::{Charge, Credit};
use super::super::records::Cause;
use super::super::roles::pilot_warnings::Warnings;
use super::super::roles::pilots::Graded;
use crate::application::task::{FindingSeverity, FindingVerdict};

/// How far back the scan reads. Every window, `all` included, fits inside it.
pub(in crate::application::agents) const SCAN_DAYS: i64 = 90;
/// The jobs whose runs implement a task: the PR and local delivery leaves,
/// owned or claimed.
pub(super) const IMPLEMENTATION_JOBS: &[&str] = &[
    "task_pr_pipeline",
    "task_local_pipeline",
    "task_claimed_pr_pipeline",
    "task_claimed_local_pipeline",
];
/// Leads of the review gate's settlement comment, before the attempt id.
pub(super) const SETTLEMENT_LEADS: &[&str] = &[
    "before-PR review settled attempt `",
    "before-landing review settled attempt `",
];
/// The settlement comment's reviewer line, before the crew name.
pub(super) const REVIEWER_LINE: &str = "- Reviewer: crew `";
/// The settlement comment's line naming the reviewed implementation commit.
pub(super) const IMPLEMENTATION_LINE: &str = "- Implementation: `";
/// The settlement comment's line naming the final candidate commit.
pub(super) const FINAL_CANDIDATE_LINE: &str = "- Final candidate: `";
/// The settlement comment's line with the reviewer's runtime in seconds.
pub(super) const RUNTIME_LINE: &str = "- Reviewer runtime: ";
/// A finding's sub-line naming the paths a fixed finding changed.
pub(super) const CHANGED_LINE: &str = "- Changed: ";
/// A finding's sub-line naming the paths an open or disposed finding cites.
pub(super) const CITED_LINE: &str = "- Cited: ";

/// A recorded occurrence that counts against some role, or against none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Problem {
    /// The description or acceptance criteria edited after the first
    /// implementation run started; counted once per task.
    SpecEdit,
    /// An agent blocker whose kind names a criterion, spec, requirement,
    /// scope or ambiguity.
    CriteriaBlock,
    /// A final-recovery escalation whose diagnosis cites the criteria.
    CriteriaEscalation,
    /// An implementation run after the task's first.
    RepeatRun,
    /// A `[BLOCKED]` PR a failure handoff published.
    BlockedPr,
    /// A delivery PR a settled task recorded but never merged.
    UnmergedPr,
    /// A blocked task returned to the backlog by hand.
    OperatorRequeue,
    /// A review gate settlement with verdict `reject`.
    ReviewReject,
    /// A review gate settlement with verdict `accept_with_fixes`.
    ReviewFixes,
    /// A typed `regression_from` link against the task.
    Regression,
    /// A review finding closed by an authorized decision.
    DisposedFinding,
    /// Reviewer fixes that failed owner revalidation and were escalated.
    FailedFix,
    /// A `reject` whose next implementation run changed none of its open
    /// findings' cited paths and whose next review accepted.
    FalseReject,
    /// A finding a review job filed as a task that finding verification
    /// REJECTED.
    RejectedFinding,
    /// A review gate settlement with verdict `incomplete`.
    IncompleteReview,
    /// A landed file no pilot selector covered that a base-conflict block or
    /// a refused lock request of the task named.
    CostlyMiss,
    /// Another task's lock wait on a path the task held only through pilot
    /// selectors that covered none of its landed files.
    LockWait,
}

impl Problem {
    /// The problem's wire key.
    pub fn as_str(self) -> &'static str {
        match self {
            Problem::SpecEdit => "spec_edit",
            Problem::CriteriaBlock => "criteria_block",
            Problem::CriteriaEscalation => "criteria_escalation",
            Problem::RepeatRun => "repeat_run",
            Problem::BlockedPr => "blocked_pr",
            Problem::UnmergedPr => "unmerged_pr",
            Problem::OperatorRequeue => "operator_requeue",
            Problem::ReviewReject => "review_reject",
            Problem::ReviewFixes => "review_fixes",
            Problem::Regression => "regression",
            Problem::DisposedFinding => "disposed_finding",
            Problem::FailedFix => "failed_fix",
            Problem::FalseReject => "false_reject",
            Problem::RejectedFinding => "rejected_finding",
            Problem::IncompleteReview => "incomplete_review",
            Problem::CostlyMiss => "costly_miss",
            Problem::LockWait => "lock_wait",
        }
    }

    /// Rework: a repeat run, a `[BLOCKED]` or unmerged PR, or a requeue.
    pub fn is_rework(self) -> bool {
        matches!(
            self,
            Problem::RepeatRun
                | Problem::BlockedPr
                | Problem::UnmergedPr
                | Problem::OperatorRequeue
        )
    }
}

/// Who did a piece of work: the crew and model a job run recorded, and the
/// run. Each is `None` when nothing records it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(in crate::application::agents) struct Runner {
    pub(in crate::application::agents) crew: Option<String>,
    pub(in crate::application::agents) model: Option<String>,
    pub(in crate::application::agents) run_id: Option<String>,
}

/// One implementation attempt: a run span from task history, and the job
/// run that worked it when one is found.
#[derive(Debug, Clone)]
pub(in crate::application::agents) struct Attempt {
    pub(in crate::application::agents) start: DateTime<Utc>,
    pub(in crate::application::agents) end: Option<DateTime<Utc>>,
    pub(in crate::application::agents) runner: Runner,
}

/// A problem as the task's records show it, before it has an owner.
#[derive(Debug, Clone)]
pub(in crate::application::agents) struct Record {
    pub(in crate::application::agents) problem: Problem,
    pub(in crate::application::agents) at: DateTime<Utc>,
    pub(in crate::application::agents) detail: String,
    /// What the rework is attributed to; `None` for anything else.
    pub(in crate::application::agents) cause: Option<Cause>,
    /// The attempt the rework ended, when the record names one.
    pub(in crate::application::agents) ended: Option<usize>,
}

/// What a review finding's settlement records about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::application::agents) enum FindingState {
    /// Left open: it blocks a `reject`.
    Open,
    /// Repaired by the reviewer.
    Fixed,
    /// Closed by an authorized decision.
    Disposed,
}

/// One finding of a settled review.
#[derive(Debug, Clone)]
pub(in crate::application::agents) struct Finding {
    pub(in crate::application::agents) severity: String,
    pub(in crate::application::agents) state: FindingState,
    /// The paths a fixed finding changed, or an open or disposed one cites.
    pub(in crate::application::agents) paths: BTreeSet<String>,
    /// The finding's settlement line: its id, severity, state and summary.
    pub(in crate::application::agents) line: String,
}

/// How the run after a `reject` turned out for the findings it left open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::application::agents) enum RejectOutcome {
    /// No review settled after it yet.
    Pending,
    /// The records cannot tell: no open finding cites a path, a settlement
    /// names no commit, or the repository lacks one of the commits.
    Unknown,
    /// The next review settled. `changed` holds the cited paths whose
    /// content differs between the rejected candidate and the next reviewed
    /// one; `accepted` is whether the next review's verdict passed.
    Judged {
        changed: BTreeSet<String>,
        accepted: bool,
    },
}

/// One settled review gate attempt, from its settlement comment.
#[derive(Debug, Clone)]
pub(in crate::application::agents) struct Review {
    pub(in crate::application::agents) attempt_id: String,
    pub(in crate::application::agents) at: DateTime<Utc>,
    pub(in crate::application::agents) verdict: ReviewVerdict,
    /// The reviewer's crew and model as the settlement names them.
    pub(in crate::application::agents) reviewer: Runner,
    pub(in crate::application::agents) findings: Vec<Finding>,
    /// The implementation commit the reviewer examined.
    pub(in crate::application::agents) reviewed: Option<String>,
    /// The candidate the verdict binds to, after any reviewer commit.
    pub(in crate::application::agents) final_candidate: Option<String>,
    /// The reviewer's runtime in seconds.
    pub(in crate::application::agents) seconds: Option<u64>,
    /// Reviewer fixes that a gate escalation says failed owner revalidation.
    pub(in crate::application::agents) fix_failed: bool,
    /// An `incomplete` verdict whose block the run failure classification
    /// names as infrastructure.
    pub(in crate::application::agents) infrastructure: bool,
    /// For a `reject`, how the next run treated its open findings.
    pub(in crate::application::agents) outcome: RejectOutcome,
}

impl Review {
    /// The findings closed by an authorized decision.
    pub(in crate::application::agents) fn disposed(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|finding| finding.state == FindingState::Disposed)
    }

    /// The findings left open.
    pub(in crate::application::agents) fn open(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|finding| finding.state == FindingState::Open)
    }

    /// A `reject` the next run proved wrong: it changed none of the cited
    /// paths and the next review accepted.
    pub(in crate::application::agents) fn false_reject(&self) -> bool {
        self.verdict == ReviewVerdict::Reject
            && matches!(&self.outcome, RejectOutcome::Judged { changed, accepted } if changed.is_empty() && *accepted)
    }
}

/// A finding a review job filed as a `bug` task, and what finding
/// verification decided about it.
#[derive(Debug, Clone)]
pub(in crate::application::agents) struct FiledFinding {
    /// The review task it was spawned from.
    pub(in crate::application::agents) filer: Option<String>,
    /// The latest verdict, with when it was recorded and the severity the
    /// verifier rated.
    pub(in crate::application::agents) verdict:
        Option<(FindingVerdict, DateTime<Utc>, Option<FindingSeverity>)>,
}

/// A typed `regression_from` link: `regressing` regressed `culprit`.
#[derive(Debug, Clone)]
pub(in crate::application::agents) struct RegressionLink {
    pub(in crate::application::agents) culprit: String,
    pub(in crate::application::agents) regressing: String,
    pub(in crate::application::agents) at: DateTime<Utc>,
    pub(in crate::application::agents) classification: RegressionClassification,
}

#[derive(Debug, Clone)]
pub(in crate::application::agents) struct TaskFacts {
    pub(in crate::application::agents) id: String,
    pub(in crate::application::agents) title: String,
    pub(in crate::application::agents) status: TaskStatus,
    /// The task's orchestrator attribution, when it names one.
    pub(in crate::application::agents) orchestrator: Option<String>,
    /// The orchestrator's model recorded when the task was created.
    pub(in crate::application::agents) orchestrator_model: Option<String>,
    pub(in crate::application::agents) origin: Option<TaskOrigin>,
    /// The task's tags, for task-type classification.
    pub(in crate::application::agents) tags: Vec<String>,
    /// The task's context-file selectors, for task-type classification.
    pub(in crate::application::agents) context_files: Vec<String>,
    pub(in crate::application::agents) complexity: Option<TaskComplexity>,
    pub(in crate::application::agents) created_at: DateTime<Utc>,
    /// Done, or archived after it was done.
    pub(in crate::application::agents) landed: bool,
    /// Rejected or archived without ever being done.
    pub(in crate::application::agents) dropped: bool,
    pub(in crate::application::agents) attempts: Vec<Attempt>,
    /// The attempt whose candidate landed: the last one started before the
    /// task was done.
    pub(in crate::application::agents) final_attempt: Option<usize>,
    /// When the task was last done.
    pub(in crate::application::agents) landed_at: Option<DateTime<Utc>>,
    pub(in crate::application::agents) records: Vec<Record>,
    pub(in crate::application::agents) reviews: Vec<Review>,
    /// Set when the task is a finding a review job filed.
    pub(in crate::application::agents) filed: Option<FiledFinding>,
    /// How each task-pilot assessment's warnings settled.
    pub(in crate::application::agents) warnings: Warnings,
}

impl TaskFacts {
    /// The attempt whose span contains `at`, else the most recent one that
    /// started before it.
    pub(in crate::application::agents) fn attempt_at(&self, at: DateTime<Utc>) -> Option<usize> {
        self.attempts
            .iter()
            .rposition(|attempt| attempt.start <= at && attempt.end.is_none_or(|end| at <= end))
            .or_else(|| {
                self.attempts
                    .iter()
                    .rposition(|attempt| attempt.start <= at)
            })
    }
}

/// The review job attempt that filed `task`: the attempt of the task it was
/// spawned from whose span contains the filing. `None` for a task no review
/// job filed, or whose filer is outside the scan or has no attempt then.
pub(in crate::application::agents) fn filing_attempt<'a>(
    by_id: &HashMap<&str, &'a TaskFacts>,
    task: &TaskFacts,
) -> Option<(&'a TaskFacts, usize)> {
    let filer = by_id.get(task.filed.as_ref()?.filer.as_deref()?)?;
    Some((filer, filer.attempt_at(task.created_at)?))
}

/// One scan of a workspace: every role and the attribution strip slice it.
#[derive(Debug)]
pub struct AgentFacts {
    pub(in crate::application::agents) computed_at: DateTime<Utc>,
    pub(in crate::application::agents) tasks_scanned: usize,
    pub(in crate::application::agents) tasks: Vec<TaskFacts>,
    pub(in crate::application::agents) charges: Vec<Charge>,
    pub(in crate::application::agents) run_costs: BTreeMap<String, Option<f64>>,
    pub(in crate::application::agents) credits: Vec<Credit>,
    /// Each pilot assessment of a scanned done task, graded.
    pub(in crate::application::agents) pilots: Vec<Graded>,
}

impl AgentFacts {
    /// Every problem the scan found, each with its one owner or its Not
    /// scored reason.
    pub fn charges(&self) -> &[Charge] {
        &self.charges
    }

    /// Landed credit, each to the implementation run whose candidate landed.
    pub fn credits(&self) -> &[Credit] {
        &self.credits
    }
}

/// A job run that implemented a task.
pub(super) struct RunRecord {
    pub(super) started_at: DateTime<Utc>,
    pub(super) finished_at: Option<DateTime<Utc>>,
    pub(super) runner: Runner,
}
