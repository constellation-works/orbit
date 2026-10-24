//! The one workspace scan behind every agent scorecard: each task with
//! activity in the last 90 days (envelope, history and comments), the
//! implementation job runs that name those tasks, the review gate's
//! settlement comments, the finding-verification verdicts on review-filed
//! findings, the typed `regression_from` links and, from the codebase scan,
//! each pilot assessment of a done task graded against its landed diff. The
//! scan keeps the records; [`super::attribution`] decides who each problem
//! counts against, and the roles only slice the result by window.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::{InvocationQuery, JobRunQuery};
use orbit_types::task::{
    GITHUB_PR_EXTERNAL_REF_SYSTEM, RegressionClassification, Task, TaskComment, TaskComplexity,
    TaskHistoryEntry, TaskOrigin, TaskRelationType, TaskStatus,
};
use orbit_types::workflow::ReviewVerdict;
use serde_json::Value;

use super::attribution::{self, Charge, Credit};
use super::candidates::Candidates;
use super::records::{
    Cause, block_cause, blocked_pr, bounded, cites_criteria, is_operator_requeue, landed,
    landing_pr, restart_cause, run_spans, spec_blocker_kind, spec_documents,
};
use super::roles::pilots::{Graded, grade_all};
use crate::OrbitRuntime;
use crate::application::codebase::{PilotEvidence, failed_claim_settlements_of, pilot_evidence};
use crate::application::task::{
    FindingSeverity, FindingVerdict, is_review_finding, latest_verification,
};

/// How far back the scan reads. Every window, `all` included, fits inside it.
pub(super) const SCAN_DAYS: i64 = 90;
/// The jobs whose runs implement a task: the PR and local delivery leaves,
/// owned or claimed.
const IMPLEMENTATION_JOBS: &[&str] = &[
    "task_pr_pipeline",
    "task_local_pipeline",
    "task_claimed_pr_pipeline",
    "task_claimed_local_pipeline",
];
/// Leads of the review gate's settlement comment, before the attempt id.
const SETTLEMENT_LEADS: &[&str] = &[
    "before-PR review settled attempt `",
    "before-landing review settled attempt `",
];
/// The settlement comment's reviewer line, before the crew name.
const REVIEWER_LINE: &str = "- Reviewer: crew `";
/// The settlement comment's line naming the reviewed implementation commit.
const IMPLEMENTATION_LINE: &str = "- Implementation: `";
/// The settlement comment's line naming the final candidate commit.
const FINAL_CANDIDATE_LINE: &str = "- Final candidate: `";
/// The settlement comment's line with the reviewer's runtime in seconds.
const RUNTIME_LINE: &str = "- Reviewer runtime: ";
/// A finding's sub-line naming the paths a fixed finding changed.
const CHANGED_LINE: &str = "- Changed: ";
/// A finding's sub-line naming the paths an open or disposed finding cites.
const CITED_LINE: &str = "- Cited: ";

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
pub(super) struct Runner {
    pub(super) crew: Option<String>,
    pub(super) model: Option<String>,
    pub(super) run_id: Option<String>,
}

/// One implementation attempt: a run span from task history, and the job
/// run that worked it when one is found.
#[derive(Debug, Clone)]
pub(super) struct Attempt {
    pub(super) start: DateTime<Utc>,
    pub(super) end: Option<DateTime<Utc>>,
    pub(super) runner: Runner,
}

/// A problem as the task's records show it, before it has an owner.
#[derive(Debug, Clone)]
pub(super) struct Record {
    pub(super) problem: Problem,
    pub(super) at: DateTime<Utc>,
    pub(super) detail: String,
    /// What the rework is attributed to; `None` for anything else.
    pub(super) cause: Option<Cause>,
    /// The attempt the rework ended, when the record names one.
    pub(super) ended: Option<usize>,
}

/// What a review finding's settlement records about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FindingState {
    /// Left open: it blocks a `reject`.
    Open,
    /// Repaired by the reviewer.
    Fixed,
    /// Closed by an authorized decision.
    Disposed,
}

/// One finding of a settled review.
#[derive(Debug, Clone)]
pub(super) struct Finding {
    pub(super) severity: String,
    pub(super) state: FindingState,
    /// The paths a fixed finding changed, or an open or disposed one cites.
    pub(super) paths: BTreeSet<String>,
    /// The finding's settlement line: its id, severity, state and summary.
    pub(super) line: String,
}

/// How the run after a `reject` turned out for the findings it left open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RejectOutcome {
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
pub(super) struct Review {
    pub(super) attempt_id: String,
    pub(super) at: DateTime<Utc>,
    pub(super) verdict: ReviewVerdict,
    /// The reviewer's crew and model as the settlement names them.
    pub(super) reviewer: Runner,
    pub(super) findings: Vec<Finding>,
    /// The implementation commit the reviewer examined.
    pub(super) reviewed: Option<String>,
    /// The candidate the verdict binds to, after any reviewer commit.
    pub(super) final_candidate: Option<String>,
    /// The reviewer's runtime in seconds.
    pub(super) seconds: Option<u64>,
    /// Reviewer fixes that a gate escalation says failed owner revalidation.
    pub(super) fix_failed: bool,
    /// An `incomplete` verdict whose block the run failure classification
    /// names as infrastructure.
    pub(super) infrastructure: bool,
    /// For a `reject`, how the next run treated its open findings.
    pub(super) outcome: RejectOutcome,
}

impl Review {
    /// The findings closed by an authorized decision.
    pub(super) fn disposed(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|finding| finding.state == FindingState::Disposed)
    }

    /// The findings left open.
    pub(super) fn open(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|finding| finding.state == FindingState::Open)
    }

    /// A `reject` the next run proved wrong: it changed none of the cited
    /// paths and the next review accepted.
    pub(super) fn false_reject(&self) -> bool {
        self.verdict == ReviewVerdict::Reject
            && matches!(&self.outcome, RejectOutcome::Judged { changed, accepted } if changed.is_empty() && *accepted)
    }
}

/// A finding a review job filed as a `bug` task, and what finding
/// verification decided about it.
#[derive(Debug, Clone)]
pub(super) struct FiledFinding {
    /// The review task it was spawned from.
    pub(super) filer: Option<String>,
    /// The latest verdict, with when it was recorded and the severity the
    /// verifier rated.
    pub(super) verdict: Option<(FindingVerdict, DateTime<Utc>, Option<FindingSeverity>)>,
}

/// A typed `regression_from` link: `regressing` regressed `culprit`.
#[derive(Debug, Clone)]
pub(super) struct RegressionLink {
    pub(super) culprit: String,
    pub(super) regressing: String,
    pub(super) at: DateTime<Utc>,
    pub(super) classification: RegressionClassification,
}

#[derive(Debug, Clone)]
pub(super) struct TaskFacts {
    pub(super) id: String,
    pub(super) title: String,
    pub(super) status: TaskStatus,
    /// The task's orchestrator attribution, when it names one.
    pub(super) orchestrator: Option<String>,
    /// The orchestrator's model recorded when the task was created.
    pub(super) orchestrator_model: Option<String>,
    pub(super) origin: Option<TaskOrigin>,
    /// The task's tags, for task-type classification.
    pub(super) tags: Vec<String>,
    /// The task's context-file selectors, for task-type classification.
    pub(super) context_files: Vec<String>,
    pub(super) complexity: Option<TaskComplexity>,
    pub(super) created_at: DateTime<Utc>,
    /// Done, or archived after it was done.
    pub(super) landed: bool,
    /// Rejected or archived without ever being done.
    pub(super) dropped: bool,
    pub(super) attempts: Vec<Attempt>,
    /// The attempt whose candidate landed: the last one started before the
    /// task was done.
    pub(super) final_attempt: Option<usize>,
    /// When the task was last done.
    pub(super) landed_at: Option<DateTime<Utc>>,
    pub(super) records: Vec<Record>,
    pub(super) reviews: Vec<Review>,
    /// Set when the task is a finding a review job filed.
    pub(super) filed: Option<FiledFinding>,
}

impl TaskFacts {
    /// The attempt whose span contains `at`, else the most recent one that
    /// started before it.
    pub(super) fn attempt_at(&self, at: DateTime<Utc>) -> Option<usize> {
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
pub(super) fn filing_attempt<'a>(
    by_id: &HashMap<&str, &'a TaskFacts>,
    task: &TaskFacts,
) -> Option<(&'a TaskFacts, usize)> {
    let filer = by_id.get(task.filed.as_ref()?.filer.as_deref()?)?;
    Some((filer, filer.attempt_at(task.created_at)?))
}

/// One scan of a workspace: every role and the attribution strip slice it.
#[derive(Debug)]
pub struct AgentFacts {
    pub(super) computed_at: DateTime<Utc>,
    pub(super) tasks_scanned: usize,
    pub(super) tasks: Vec<TaskFacts>,
    pub(super) charges: Vec<Charge>,
    pub(super) run_costs: BTreeMap<String, Option<f64>>,
    pub(super) credits: Vec<Credit>,
    /// Each pilot assessment of a scanned done task, graded.
    pub(super) pilots: Vec<Graded>,
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
struct RunRecord {
    started_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
    runner: Runner,
}

/// Scan the workspace once. A task whose history or comments cannot be read
/// keeps its envelope facts rather than failing the whole view.
pub(super) fn scan(runtime: &OrbitRuntime) -> Result<AgentFacts, OrbitError> {
    let computed_at = Utc::now();
    let horizon = computed_at - Duration::days(SCAN_DAYS);
    let tasks = runtime.list_task_metadata()?;
    let tasks_scanned = tasks.len();

    let mut regressions = Vec::new();
    for task in &tasks {
        for relation in &task.relations {
            if relation.relation_type == TaskRelationType::RegressionFrom
                && let Some(classification) = relation.regression
            {
                regressions.push(RegressionLink {
                    culprit: relation.target.to_string(),
                    regressing: task.id.to_string(),
                    at: task.created_at,
                    classification,
                });
            }
        }
    }

    let mut runs = implementation_runs(runtime, horizon);
    let run_costs = run_costs(runtime, horizon, computed_at);
    let leaves = runtime.settled_leaves(false).unwrap_or_else(|error| {
        tracing::warn!(%error, "agent scorecards: claim settlements unreadable");
        Vec::new()
    });
    let settlements = failed_claim_settlements_of(&leaves);
    let pulled: HashMap<&str, Runner> = leaves
        .iter()
        .map(|leaf| {
            let runner = Runner {
                crew: leaf.crew.clone(),
                model: leaf.crew_model.clone(),
                run_id: None,
            };
            (leaf.claim_id.as_str(), runner)
        })
        .collect();
    let candidates = Candidates::open(runtime);
    let mut facts = Vec::new();
    for task in tasks {
        if task.created_at < horizon && task.updated_at < horizon {
            continue;
        }
        let id = task.id.to_string();
        let history = runtime.get_task_history(&id).unwrap_or_else(|error| {
            tracing::warn!(task_id = %id, %error, "agent scorecards: task history unreadable");
            Vec::new()
        });
        let comments = runtime.get_task_comments(&id).unwrap_or_else(|error| {
            tracing::warn!(task_id = %id, %error, "agent scorecards: task comments unreadable");
            Vec::new()
        });
        let task_settlements = settlements.get(&id).map(Vec::as_slice).unwrap_or_default();
        let classify = |entry: &TaskHistoryEntry| block_cause(entry, task_settlements);
        let task_runs = runs.remove(&id).unwrap_or_default();
        let mut task_facts = task_facts(
            &task,
            &history,
            &comments,
            &task_runs,
            &pulled,
            &classify,
            computed_at,
        );
        judge_rejects(&mut task_facts.reviews, &candidates);
        facts.push(task_facts);
    }

    let scanned: BTreeSet<&str> = facts.iter().map(|task| task.id.as_str()).collect();
    let mut pilots = grade_all(pilot_scan(runtime));
    pilots.retain(|graded| scanned.contains(graded.task_id.as_str()));
    let (charges, credits) = attribution::attribute(&facts, &regressions, &pilots);
    Ok(AgentFacts {
        computed_at,
        tasks_scanned,
        tasks: facts,
        charges,
        run_costs,
        credits,
        pilots,
    })
}

/// The pilot evidence of the codebase scan, which keeps each task's landed
/// diff and receipts between scans. A scan that fails leaves the pilots
/// unscored rather than failing every scorecard.
fn pilot_scan(runtime: &OrbitRuntime) -> PilotEvidence {
    match runtime.codebase_scan() {
        Ok(codebase) => pilot_evidence(runtime, &codebase),
        Err(error) => {
            tracing::warn!(%error, "agent scorecards: codebase scan failed; pilots unscored");
            PilotEvidence::default()
        }
    }
}

/// The same provider/derived telemetry as Cost, grouped by workspace-local
/// delivery run. Missing prices or a truncated read never become zero spend.
fn run_costs(
    runtime: &OrbitRuntime,
    horizon: DateTime<Utc>,
    until: DateTime<Utc>,
) -> BTreeMap<String, Option<f64>> {
    const LIMIT: usize = 100_000;
    let records = match runtime.workspace_invocation_records(InvocationQuery {
        since: Some(horizon),
        until: Some(until),
        limit: LIMIT,
        ..Default::default()
    }) {
        Ok(records) if records.len() < LIMIT => records,
        Ok(_) => {
            tracing::warn!("agent scorecards: cost scan truncated; costs unavailable");
            return BTreeMap::new();
        }
        Err(error) => {
            tracing::warn!(%error, "agent scorecards: costs unreadable");
            return BTreeMap::new();
        }
    };
    let mut costs = BTreeMap::new();
    for record in records {
        let cost = record
            .provider_cost_usd
            .or(record.derived_cost_usd)
            .filter(|cost| cost.is_finite() && *cost >= 0.0);
        let total = costs.entry(record.job_run_id).or_insert(Some(0.0));
        *total = total.zip(cost).map(|(sum, cost)| sum + cost);
    }
    costs
}

/// The implementation job runs active since `horizon`, by the task their
/// input names, oldest first. Read without their steps.
fn implementation_runs(
    runtime: &OrbitRuntime,
    horizon: DateTime<Utc>,
) -> BTreeMap<String, Vec<RunRecord>> {
    let mut by_task: BTreeMap<String, Vec<RunRecord>> = BTreeMap::new();
    for job in IMPLEMENTATION_JOBS {
        let runs = runtime
            .stores()
            .jobs()
            .list_job_runs_filtered(&JobRunQuery {
                job_id: Some((*job).to_string()),
                recency_since: Some(horizon),
                include_steps: false,
                ..Default::default()
            })
            .unwrap_or_else(|error| {
                tracing::warn!(job, %error, "agent scorecards: implementation runs unreadable");
                Vec::new()
            });
        for run in runs {
            let Some(input) = run.input.as_ref() else {
                continue;
            };
            let named: BTreeSet<&str> = input
                .get("task_ids")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .chain(input.get("task_id").and_then(Value::as_str))
                .collect();
            for task_id in named {
                by_task
                    .entry(task_id.to_string())
                    .or_default()
                    .push(RunRecord {
                        started_at: run.started_at.unwrap_or(run.created_at),
                        finished_at: run.finished_at,
                        runner: Runner {
                            crew: run.resolved_crew.clone(),
                            model: run.crew_model.clone(),
                            run_id: Some(run.run_id.to_string()),
                        },
                    });
            }
        }
    }
    for runs in by_task.values_mut() {
        runs.sort_by_key(|run| run.started_at);
    }
    by_task
}

/// The job run that worked an attempt starting at `start`: the latest run
/// that started by then and had not finished, else the first that started
/// inside the attempt. `None` when no local run worked it.
fn runner(runs: &[RunRecord], start: DateTime<Utc>, end: Option<DateTime<Utc>>) -> Option<Runner> {
    runs.iter()
        .rev()
        .find(|run| run.started_at <= start && run.finished_at.is_none_or(|done| done >= start))
        .or_else(|| {
            runs.iter()
                .find(|run| run.started_at >= start && end.is_none_or(|end| run.started_at <= end))
        })
        .map(|run| run.runner.clone())
}

/// Who worked an attempt with no local job run, as a pull from another
/// machine is: the crew and model on the settlement record of the claim the
/// `pulled_by` note names, from `pulled` by claim, and the pulling run that
/// note names. Without a record, or a field it lacks, the crew falls back to
/// the one task history had assigned by the attempt's start (else the task's
/// crew when history records no assignment) and the model stays unknown.
fn assigned_runner(
    task: &Task,
    history: &[TaskHistoryEntry],
    pulled: &HashMap<&str, Runner>,
    start: DateTime<Utc>,
) -> Runner {
    let note = history
        .iter()
        .find(|entry| entry.at == start && entry.event == "pulled_by")
        .and_then(|entry| entry.note.as_deref())
        .and_then(|note| serde_json::from_str::<Value>(note).ok());
    let field = |pointer: &str| {
        note.as_ref()
            .and_then(|note| note.pointer(pointer))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let settled = field("/claim_id").and_then(|claim| pulled.get(claim.as_str()));
    let assignments = || {
        history
            .iter()
            .filter(|entry| matches!(entry.event.as_str(), "crew_assigned" | "crew_redrawn"))
    };
    let crew = match assignments().rfind(|entry| entry.at <= start) {
        Some(entry) => entry.note.as_deref().and_then(assigned_crew),
        None if assignments().next().is_none() => task.crew.clone(),
        None => None,
    };
    Runner {
        crew: settled.and_then(|runner| runner.crew.clone()).or(crew),
        model: settled.and_then(|runner| runner.model.clone()),
        run_id: field("/run_context/run_id"),
    }
}

/// The crew an assignment note ends on: its last backticked name, which every
/// assignment and redraw note gives last. A placeholder such as `(none)` is no
/// crew.
fn assigned_crew(note: &str) -> Option<String> {
    let last = note.split('`').skip(1).step_by(2).last()?;
    (!last.is_empty() && !last.starts_with(['(', '<'])).then(|| last.to_string())
}

/// The facts of one task. `classify` gives the cause a block or run exit
/// records.
fn task_facts(
    task: &Task,
    history: &[TaskHistoryEntry],
    comments: &[TaskComment],
    runs: &[RunRecord],
    pulled: &HashMap<&str, Runner>,
    classify: &dyn Fn(&TaskHistoryEntry) -> Cause,
    now: DateTime<Utc>,
) -> TaskFacts {
    let spans = run_spans(history);
    let attempts: Vec<Attempt> = spans
        .iter()
        .map(|span| Attempt {
            start: span.start,
            end: span.end,
            runner: runner(runs, span.start, span.end.or(Some(now)))
                .unwrap_or_else(|| assigned_runner(task, history, pulled, span.start)),
        })
        .collect();
    let landed = landed(task.status, history);
    let dropped = matches!(task.status, TaskStatus::Archived | TaskStatus::Rejected) && !landed;
    let landed_at = landed
        .then(|| {
            history
                .iter()
                .rev()
                .find(|entry| entry.to_status == Some(TaskStatus::Done))
                .map(|entry| entry.at)
        })
        .flatten();
    let final_attempt = landed
        .then(|| match landed_at {
            Some(at) => attempts.iter().rposition(|attempt| attempt.start <= at),
            None => attempts.len().checked_sub(1),
        })
        .flatten();
    let mut records = Vec::new();

    // Spec defects recorded on the task's own history. An edited task counts
    // once, however many edits it had.
    let first_start = attempts.first().map(|attempt| attempt.start);
    let mut edits = history.iter().filter_map(|entry| {
        let documents = spec_documents(entry)?;
        first_start
            .is_some_and(|first| entry.at > first)
            .then_some((entry, documents))
    });
    if let Some((entry, documents)) = edits.next() {
        let later = edits.count();
        records.push(Record {
            problem: Problem::SpecEdit,
            at: entry.at,
            detail: if later == 0 {
                format!("{} edited by {}", documents.join(" and "), entry.by)
            } else {
                format!(
                    "{} edited by {}, then {later} more edit(s)",
                    documents.join(" and "),
                    entry.by
                )
            },
            cause: None,
            ended: None,
        });
    }
    for entry in history {
        if let Some(kind) = spec_blocker_kind(entry) {
            records.push(Record {
                problem: Problem::CriteriaBlock,
                at: entry.at,
                detail: format!("agent blocker kind {kind}"),
                cause: None,
                ended: None,
            });
        }
        if cites_criteria(entry) {
            records.push(Record {
                problem: Problem::CriteriaEscalation,
                at: entry.at,
                detail: bounded(entry.note.as_deref().unwrap_or_default()),
                cause: None,
                ended: None,
            });
        }
    }

    // `[BLOCKED]` PRs, then delivery PRs that a settled task never merged.
    let mut blocked_prs = BTreeSet::new();
    for entry in history {
        let Some(number) = blocked_pr(entry) else {
            continue;
        };
        if blocked_prs.insert(number.to_string()) {
            records.push(Record {
                problem: Problem::BlockedPr,
                at: entry.at,
                detail: format!("[BLOCKED] PR #{number}"),
                cause: Some(classify(entry)),
                ended: None,
            });
        }
    }
    let landing = landed.then(|| landing_pr(history)).flatten();
    if dropped || landing.is_some() {
        let settled_at = history
            .iter()
            .rev()
            .find(|entry| entry.to_status.is_some_and(TaskStatus::is_terminal))
            .map_or(task.updated_at, |entry| entry.at);
        for reference in &task.external_refs {
            if reference.system != GITHUB_PR_EXTERNAL_REF_SYSTEM
                || blocked_prs.contains(&reference.id)
                || landing.as_deref() == Some(reference.id.as_str())
            {
                continue;
            }
            records.push(Record {
                problem: Problem::UnmergedPr,
                at: settled_at,
                detail: format!("PR #{} closed without merging", reference.id),
                cause: Some(Cause::Other),
                ended: None,
            });
        }
    }

    // Each run after the first, attributed to what ended the one before it.
    for (index, pair) in attempts.windows(2).enumerate() {
        let [previous, attempt] = pair else { continue };
        records.push(Record {
            problem: Problem::RepeatRun,
            at: attempt.start,
            detail: format!("run {} of {}", index + 2, attempts.len()),
            cause: Some(restart_cause(
                history,
                Some(previous.start),
                attempt.start,
                classify,
            )),
            ended: Some(index),
        });
    }

    // Blocked tasks returned to the backlog by hand.
    for entry in history {
        if !is_operator_requeue(entry) {
            continue;
        }
        let ended = attempts
            .iter()
            .rposition(|attempt| attempt.start < entry.at);
        records.push(Record {
            problem: Problem::OperatorRequeue,
            at: entry.at,
            detail: format!("{} by {}", entry.event, entry.by),
            cause: Some(restart_cause(
                history,
                ended.map(|index| attempts[index].start),
                entry.at,
                classify,
            )),
            ended,
        });
    }

    // Review gate settlements and escalations of reviewer fixes, from the
    // comments the gate posts. A replayed settlement is counted once.
    let mut reviews: Vec<Review> = Vec::new();
    for comment in comments {
        if let Some(review) = settlement(comment) {
            if !reviews
                .iter()
                .any(|seen| seen.attempt_id == review.attempt_id)
            {
                reviews.push(review);
            }
        } else if failed_fix(comment) {
            if let Some(review) = reviews.iter_mut().rfind(|review| review.at <= comment.at) {
                review.fix_failed = true;
            }
            records.push(Record {
                problem: Problem::FailedFix,
                at: comment.at,
                detail: "reviewer fixes failed owner revalidation".to_string(),
                cause: None,
                ended: None,
            });
        }
    }
    // An incomplete review is infrastructure's when the block that followed
    // its settlement is.
    for review in &mut reviews {
        if review.verdict == ReviewVerdict::Incomplete {
            review.infrastructure = history
                .iter()
                .filter(|entry| {
                    entry.at >= review.at
                        && entry
                            .to_status
                            .is_some_and(|status| status != TaskStatus::InProgress)
                })
                .min_by_key(|entry| entry.at)
                .is_some_and(|entry| classify(entry) == Cause::Infrastructure);
        }
    }
    let filed = is_review_finding(task).then(|| FiledFinding {
        filer: task
            .relations
            .iter()
            .find(|relation| relation.relation_type == TaskRelationType::SpawnedFrom)
            .map(|relation| relation.target.to_string()),
        verdict: latest_verification(history)
            .map(|record| (record.verdict, record.verified_at, record.severity)),
    });

    records.sort_by_key(|record| record.at);
    TaskFacts {
        id: task.id.to_string(),
        title: task.title.clone(),
        status: task.status,
        orchestrator: task
            .orchestrator
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string),
        orchestrator_model: task
            .orchestrator_model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .map(str::to_string),
        origin: task.origin,
        tags: task.tags.clone(),
        context_files: task.context_files.clone(),
        complexity: task.complexity,
        created_at: task.created_at,
        landed,
        dropped,
        attempts,
        final_attempt,
        landed_at,
        records,
        reviews,
        filed,
    }
}

/// Judge each `reject` by the review that settled after it: which of its
/// open findings' cited paths the next implementation changed, and whether
/// that next review passed.
fn judge_rejects(reviews: &mut [Review], candidates: &Candidates) {
    for index in 0..reviews.len() {
        if reviews[index].verdict != ReviewVerdict::Reject {
            continue;
        }
        let review = &reviews[index];
        let cited: BTreeSet<String> = review
            .open()
            .flat_map(|finding| finding.paths.iter().cloned())
            .collect();
        let outcome = match reviews.get(index + 1) {
            None => RejectOutcome::Pending,
            Some(_) if cited.is_empty() => RejectOutcome::Unknown,
            Some(next) => {
                let from = review.final_candidate.as_ref().or(review.reviewed.as_ref());
                match from
                    .zip(next.reviewed.as_ref())
                    .and_then(|(from, to)| candidates.changed(from, to, &cited))
                {
                    Some(changed) => RejectOutcome::Judged {
                        changed,
                        accepted: next.verdict.passed(),
                    },
                    None => RejectOutcome::Unknown,
                }
            }
        };
        reviews[index].outcome = outcome;
    }
}

/// A review gate settlement comment: its attempt, verdict, reviewer, the
/// commits it names, its runtime and its findings.
fn settlement(comment: &TaskComment) -> Option<Review> {
    let message = comment.message.trim_start();
    let rest = SETTLEMENT_LEADS
        .iter()
        .find_map(|lead| message.strip_prefix(lead))?;
    let (attempt_id, rest) = rest.split_once('`')?;
    let verdict = rest.strip_prefix(": verdict **")?.split_once("**")?.0;
    let verdict: ReviewVerdict = serde_json::from_value(Value::from(verdict)).ok()?;
    let mut review = Review {
        attempt_id: attempt_id.to_string(),
        at: comment.at,
        verdict,
        reviewer: Runner::default(),
        findings: Vec::new(),
        reviewed: None,
        final_candidate: None,
        seconds: None,
        fix_failed: false,
        infrastructure: false,
        outcome: RejectOutcome::Pending,
    };
    for line in message.lines() {
        if let Some(rest) = line.strip_prefix(REVIEWER_LINE)
            && let Some((crew, rest)) = rest.split_once('`')
        {
            review.reviewer.crew = Some(crew.to_string());
            review.reviewer.model = rest
                .split_once(" / ")
                .and_then(|(_, model)| model.split_once(')'))
                .map(|(model, _)| model.trim().to_string())
                .filter(|model| !model.is_empty());
        } else if let Some(rest) = line.strip_prefix(IMPLEMENTATION_LINE) {
            review.reviewed = commit(rest);
        } else if let Some(rest) = line.strip_prefix(FINAL_CANDIDATE_LINE) {
            review.final_candidate = commit(rest);
        } else if let Some(rest) = line.strip_prefix(RUNTIME_LINE) {
            review.seconds = rest
                .split_once('s')
                .and_then(|(seconds, _)| seconds.trim().parse().ok());
        } else if let Some(finding) = finding(line) {
            review.findings.push(finding);
        } else if let Some(sub) = line.strip_prefix("  ")
            && let Some(finding) = review.findings.last_mut()
        {
            match finding.state {
                FindingState::Fixed => {
                    if let Some(change) = sub.strip_prefix(CHANGED_LINE) {
                        finding.paths = changed_paths(change);
                    }
                }
                FindingState::Open | FindingState::Disposed => {
                    if let Some(cited) = sub.strip_prefix(CITED_LINE) {
                        finding.paths = path_list(cited);
                    }
                }
            }
        }
    }
    Some(review)
}

/// A settlement's finding line: ``- `<id>` [<severity>, <state>] <summary>``.
fn finding(line: &str) -> Option<Finding> {
    let rest = line.strip_prefix("- `")?;
    let (_, rest) = rest.split_once("` [")?;
    let (severity, rest) = rest.split_once(", ")?;
    let state = if rest.starts_with("open]") {
        FindingState::Open
    } else if rest.starts_with("fixed]") {
        FindingState::Fixed
    } else if rest.starts_with("disposed: ") {
        FindingState::Disposed
    } else {
        return None;
    };
    Some(Finding {
        severity: severity.trim().to_ascii_lowercase(),
        state,
        paths: BTreeSet::new(),
        line: bounded(line.trim_start_matches("- ")),
    })
}

/// The commit a settlement line opens with, between backticks.
fn commit(rest: &str) -> Option<String> {
    let (commit, _) = rest.split_once('`')?;
    (!commit.is_empty()).then(|| commit.to_string())
}

/// The paths a fixed finding's `Changed:` line ends on, in parentheses.
fn changed_paths(change: &str) -> BTreeSet<String> {
    change
        .strip_suffix(')')
        .and_then(|change| change.rsplit_once(" ("))
        .map(|(_, paths)| path_list(paths))
        .unwrap_or_default()
}

/// A comma-separated path list.
fn path_list(paths: &str) -> BTreeSet<String> {
    paths
        .split(", ")
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .collect()
}

/// The gate escalation of reviewer fixes that failed owner revalidation.
fn failed_fix(comment: &TaskComment) -> bool {
    comment.message.contains("## Review gate escalation")
        && comment.message.contains("Verdict: `reject`")
}
