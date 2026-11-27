use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, Utc};
use orbit_types::task::{
    DefectSeverity, RegressionClassification, TaskComplexity, TaskStatus, is_no_diff_expected,
};
use orbit_types::workflow::ReviewVerdict;
use serde::{Deserialize, Serialize};

use super::invocations::AgentInvocationFacts;
use super::launches::AgentLaunchFacts;
use crate::app::agent::attribution::{Charge, Credit};
use crate::app::agent::record::Cause;
use crate::app::agent::role::pilot_warnings::Warnings;
use crate::app::agent::role::pilots::Graded;
use crate::application::task::FindingVerdict;

/// How far back the scan reads. Every window, `all` included, fits inside it.
pub(in crate::app::agent) const SCAN_DAYS: i64 = 90;
/// A recorded occurrence that counts against some role, or against none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
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
    pub(crate) fn is_rework(self) -> bool {
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::app::agent) struct Runner {
    pub(in crate::app::agent) crew: Option<String>,
    pub(in crate::app::agent) model: Option<String>,
    pub(in crate::app::agent) run_id: Option<String>,
    /// The reasoning effort a pulled attempt's follower reported on its claim
    /// settlement, in the shared encoding (`Some(None)` is the provider
    /// default); `None` when it reported none. Used only when the owner holds
    /// no invocation evidence of its own for the attempt's run. Serialized in
    /// three states: absent is `None`, `null` is `Some(None)`, a name is
    /// `Some(Some(name))`; a derived `Option<Option<_>>` writes both `None`
    /// and `Some(None)` as `null` and the stored-facts cache would lose the
    /// provider default on reload [ORB-24640].
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "reported_effort_three_state"
    )]
    pub(in crate::app::agent) reported_effort: Option<Option<String>>,
}

/// One implementation attempt: a run span from task history, and the job
/// run that worked it when one is found.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(in crate::app::agent) struct Attempt {
    pub(in crate::app::agent) start: DateTime<Utc>,
    pub(in crate::app::agent) end: Option<DateTime<Utc>>,
    pub(in crate::app::agent) runner: Runner,
    /// Whether a job worked the attempt: a local implementation run matched
    /// it, or a claim's `pulled_by` opened it on another machine. An attempt
    /// an interactive session opened by moving the task has neither.
    pub(in crate::app::agent) job: bool,
    /// Whether a local implementation run matched it, so this machine
    /// holds the run's own records.
    pub(in crate::app::agent) local: bool,
    /// Whether the attempt is an implementer unit. `false` only when its
    /// local run provably launched no implementer (see
    /// [`super::implementer`]).
    pub(in crate::app::agent) implementer: bool,
    /// The task's tags as of this attempt's start, replayed from its
    /// `tags_changed` history: a tag added later does not reclassify work
    /// already admitted.
    pub(in crate::app::agent) tags: Vec<String>,
}

impl Attempt {
    /// Whether the task was `no-diff-expected` work when this attempt was
    /// admitted: not implementation, so it counts on no implementer row.
    pub(in crate::app::agent) fn no_diff(&self) -> bool {
        is_no_diff_expected(&self.tags)
    }

    /// Whether this attempt counts as implementation work: an implementer
    /// unit that was not `no-diff-expected` work when admitted.
    pub(in crate::app::agent) fn implements(&self) -> bool {
        self.implementer && !self.no_diff()
    }
}

/// How a task was executed, derived from its implementation attempts; no
/// agent sets it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ExecutionMode {
    /// Its final implementation attempt (the landed one, else the latest)
    /// was worked by a job.
    Managed,
    /// It reached review or done, but its final attempt had no job: the work
    /// was done in an interactive session, which records no run telemetry.
    Interactive,
    /// Neither yet: not implemented, or worked interactively and not yet in
    /// review. Counted in no execution population.
    Unclassified,
}

#[cfg(feature = "test-support")]
impl ExecutionMode {
    /// The mode's wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            ExecutionMode::Managed => "managed",
            ExecutionMode::Interactive => "interactive",
            ExecutionMode::Unclassified => "unclassified",
        }
    }
}

/// A problem as the task's records show it, before it has an owner.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(in crate::app::agent) struct Record {
    pub(in crate::app::agent) problem: Problem,
    pub(in crate::app::agent) at: DateTime<Utc>,
    pub(in crate::app::agent) detail: String,
    /// What the rework is attributed to; `None` for anything else.
    pub(in crate::app::agent) cause: Option<Cause>,
    /// The attempt the rework ended, when the record names one.
    pub(in crate::app::agent) ended: Option<usize>,
}

/// What a review finding's settlement records about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::app::agent) enum FindingState {
    /// Left open: it blocks a `reject`.
    Open,
    /// Repaired by the reviewer.
    Fixed,
    /// Closed by an authorized decision.
    Disposed,
}

/// One finding of a settled review.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(in crate::app::agent) struct Finding {
    pub(in crate::app::agent) severity: String,
    pub(in crate::app::agent) state: FindingState,
    /// The paths a fixed finding changed, or an open or disposed one cites.
    pub(in crate::app::agent) paths: BTreeSet<String>,
    /// The finding's settlement line: its id, severity, state and summary.
    pub(in crate::app::agent) line: String,
}

/// How the run after a `reject` turned out for the findings it left open.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::app::agent) enum RejectOutcome {
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

/// One settled review gate attempt, read at the task history boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(in crate::app::agent) struct Review {
    pub(in crate::app::agent) attempt_id: String,
    /// The review step that produced the settlement, as invocations
    /// record it (`review`, `landing_review`); `None` when the
    /// settlement names no known gate.
    pub(in crate::app::agent) activity_id: Option<String>,
    pub(in crate::app::agent) at: DateTime<Utc>,
    pub(in crate::app::agent) verdict: ReviewVerdict,
    /// The reviewer's crew and model as the settlement names them.
    pub(in crate::app::agent) reviewer: Runner,
    pub(in crate::app::agent) findings: Vec<Finding>,
    /// The implementation commit the reviewer examined.
    pub(in crate::app::agent) reviewed: Option<String>,
    /// The candidate the verdict binds to, after any reviewer commit.
    pub(in crate::app::agent) final_candidate: Option<String>,
    /// The reviewer's runtime in seconds.
    pub(in crate::app::agent) seconds: Option<u64>,
    /// Reviewer fixes that a gate escalation says failed owner revalidation.
    pub(in crate::app::agent) fix_failed: bool,
    /// An `incomplete` verdict whose block the run failure classification
    /// names as infrastructure.
    pub(in crate::app::agent) infrastructure: bool,
    /// For a `reject`, how the next run treated its open findings.
    pub(in crate::app::agent) outcome: RejectOutcome,
}

impl Review {
    /// The findings closed by an authorized decision.
    pub(in crate::app::agent) fn disposed(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|finding| finding.state == FindingState::Disposed)
    }

    /// The findings left open.
    pub(in crate::app::agent) fn open(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|finding| finding.state == FindingState::Open)
    }

    /// A `reject` the next run proved wrong: it changed none of the cited
    /// paths and the next review accepted.
    pub(in crate::app::agent) fn false_reject(&self) -> bool {
        self.verdict == ReviewVerdict::Reject
            && matches!(&self.outcome, RejectOutcome::Judged { changed, accepted } if changed.is_empty() && *accepted)
    }
}

/// A finding a review job filed as a `bug` task, and what finding
/// verification decided about it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(in crate::app::agent) struct FiledFinding {
    /// The review task it was spawned from.
    pub(in crate::app::agent) filer: Option<String>,
    /// The latest verdict, with when it was recorded and the severity the
    /// verifier rated.
    pub(in crate::app::agent) verdict:
        Option<(FindingVerdict, DateTime<Utc>, Option<DefectSeverity>)>,
    /// The highest severity rated on the finding's own `regression_from`
    /// links: the one record of a finding's severity outside verification.
    /// `None` when it has no link, or only links stored without a severity.
    pub(in crate::app::agent) severity: Option<DefectSeverity>,
}

/// A typed `regression_from` link: `regressing` regressed `culprit`.
#[derive(Debug, Clone)]
pub(in crate::app::agent) struct RegressionLink {
    pub(in crate::app::agent) culprit: String,
    pub(in crate::app::agent) regressing: String,
    pub(in crate::app::agent) at: DateTime<Utc>,
    pub(in crate::app::agent) classification: RegressionClassification,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(in crate::app::agent) struct TaskFacts {
    pub(in crate::app::agent) id: String,
    pub(in crate::app::agent) title: String,
    pub(in crate::app::agent) status: TaskStatus,
    /// The task's tags, for task-type classification.
    pub(in crate::app::agent) tags: Vec<String>,
    /// The task's context-file selectors, for task-type classification.
    pub(in crate::app::agent) context_files: Vec<String>,
    pub(in crate::app::agent) complexity: Option<TaskComplexity>,
    pub(in crate::app::agent) created_at: DateTime<Utc>,
    /// Done, or archived after it was done.
    pub(in crate::app::agent) landed: bool,
    /// Rejected or archived without ever being done.
    pub(in crate::app::agent) dropped: bool,
    pub(in crate::app::agent) attempts: Vec<Attempt>,
    /// The attempt whose candidate landed: the last one started before the
    /// task was done.
    pub(in crate::app::agent) final_attempt: Option<usize>,
    /// Managed, interactive or unclassified, from the attempts.
    pub(in crate::app::agent) execution: ExecutionMode,
    /// When the task was last done.
    pub(in crate::app::agent) landed_at: Option<DateTime<Utc>>,
    pub(in crate::app::agent) records: Vec<Record>,
    pub(in crate::app::agent) reviews: Vec<Review>,
    /// Set when the task is a finding a review job filed.
    pub(in crate::app::agent) filed: Option<FiledFinding>,
    /// How each task-pilot assessment's warnings settled.
    pub(in crate::app::agent) warnings: Warnings,
}

impl TaskFacts {
    /// The attempt whose span contains `at`, else the most recent one that
    /// started before it.
    pub(in crate::app::agent) fn attempt_at(&self, at: DateTime<Utc>) -> Option<usize> {
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

/// One scan of a workspace: every role and the attribution strip slice it.
#[derive(Debug)]
pub struct AgentFacts {
    pub(in crate::app::agent) invocations: AgentInvocationFacts,
    /// Launch records of the scan window, including launches whose
    /// invocation never completed.
    pub(in crate::app::agent) launches: AgentLaunchFacts,
    pub(in crate::app::agent) computed_at: DateTime<Utc>,
    pub(in crate::app::agent) tasks_scanned: usize,
    pub(in crate::app::agent) tasks: Vec<TaskFacts>,
    pub(in crate::app::agent) charges: Vec<Charge>,
    pub(in crate::app::agent) run_costs: BTreeMap<String, Option<f64>>,
    pub(in crate::app::agent) credits: Vec<Credit>,
    /// Every typed `regression_from` link in the workspace, each observed
    /// when its regressing task was created.
    pub(in crate::app::agent) regressions: Vec<RegressionLink>,
    /// Lines added plus deleted from the experiment outcome's landed diff rows.
    pub(in crate::app::agent) landed_lines: BTreeMap<String, u64>,
    /// The configured task types used for comparable diff sizes.
    pub(in crate::app::agent) performance_types: Vec<orbit_config::TaskTypeDefinition>,
    /// Each pilot assessment of a scanned done task, graded.
    pub(in crate::app::agent) pilots: Vec<Graded>,
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::app::agent) struct RunRecord {
    pub(super) started_at: DateTime<Utc>,
    pub(super) finished_at: Option<DateTime<Utc>>,
    pub(super) runner: Runner,
}

/// Deserialize a present reported effort, `null` included, as `Some`; an
/// absent one stays `None` through `#[serde(default)]`. The same three-state
/// contract as the claim settlement's reported effort.
fn reported_effort_three_state<'de, D>(deserializer: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(Some)
}

/// The review job attempt that filed `task`: the attempt of the task it was
/// spawned from whose span contains the filing. `None` for a task no review
/// job filed, or whose filer is outside the scan or has no attempt then.
pub(in crate::app::agent) fn filing_attempt<'a>(
    by_id: &HashMap<&str, &'a TaskFacts>,
    task: &TaskFacts,
) -> Option<(&'a TaskFacts, usize)> {
    let filer = by_id.get(task.filed.as_ref()?.filer.as_deref()?)?;
    Some((filer, filer.attempt_at(task.created_at)?))
}
