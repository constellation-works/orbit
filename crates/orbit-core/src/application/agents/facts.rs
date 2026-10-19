//! The one workspace scan behind every agent scorecard: each task with
//! activity in the last 90 days (envelope, history and comments), the
//! implementation job runs that name those tasks, the review gate's
//! settlement comments and the typed `regression_from` links. The scan keeps
//! the records; [`super::attribution`] decides who each problem counts
//! against, and the roles only slice the result by window.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::JobRunQuery;
use orbit_types::task::{
    GITHUB_PR_EXTERNAL_REF_SYSTEM, RegressionClassification, Task, TaskComment, TaskComplexity,
    TaskHistoryEntry, TaskOrigin, TaskRelationType, TaskStatus,
};
use orbit_types::workflow::ReviewVerdict;
use serde_json::Value;

use super::attribution::{self, Charge, Credit};
use super::records::{
    Cause, block_cause, blocked_pr, bounded, cites_criteria, is_operator_requeue, landed,
    landing_pr, restart_cause, run_spans, spec_blocker_kind, spec_documents,
};
use crate::OrbitRuntime;
use crate::application::codebase::failed_claim_settlements;

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

/// One settled review gate attempt, from its settlement comment.
#[derive(Debug, Clone)]
pub(super) struct Review {
    pub(super) attempt_id: String,
    pub(super) at: DateTime<Utc>,
    pub(super) verdict: ReviewVerdict,
    /// The reviewer's crew and model as the settlement names them.
    pub(super) reviewer: Runner,
    /// Findings closed by an authorized decision, each as one line.
    pub(super) disposed: Vec<String>,
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
    pub(super) origin: Option<TaskOrigin>,
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

/// One scan of a workspace: every role and the attribution strip slice it.
#[derive(Debug)]
pub struct AgentFacts {
    pub(super) computed_at: DateTime<Utc>,
    pub(super) tasks_scanned: usize,
    pub(super) tasks: Vec<TaskFacts>,
    pub(super) charges: Vec<Charge>,
    pub(super) credits: Vec<Credit>,
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
    let settlements = failed_claim_settlements(runtime);
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
        facts.push(task_facts(
            &task,
            &history,
            &comments,
            &task_runs,
            &classify,
            computed_at,
        ));
    }

    let (charges, credits) = attribution::attribute(&facts, &regressions);
    Ok(AgentFacts {
        computed_at,
        tasks_scanned,
        tasks: facts,
        charges,
        credits,
    })
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
/// machine is: the crew task history had assigned by its start (else the
/// task's crew when history records no assignment) and the pulling run the
/// `pulled_by` note names. No model is recorded, so the row is `model unknown`.
fn assigned_runner(task: &Task, history: &[TaskHistoryEntry], start: DateTime<Utc>) -> Runner {
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
    let run_id = history
        .iter()
        .find(|entry| entry.at == start && entry.event == "pulled_by")
        .and_then(|entry| entry.note.as_deref())
        .and_then(|note| serde_json::from_str::<Value>(note).ok())
        .and_then(|note| {
            note.pointer("/run_context/run_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    Runner {
        crew,
        model: None,
        run_id,
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
                .unwrap_or_else(|| assigned_runner(task, history, span.start)),
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
            records.push(Record {
                problem: Problem::FailedFix,
                at: comment.at,
                detail: "reviewer fixes failed owner revalidation".to_string(),
                cause: None,
                ended: None,
            });
        }
    }

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
        origin: task.origin,
        complexity: task.complexity,
        created_at: task.created_at,
        landed,
        dropped,
        attempts,
        final_attempt,
        landed_at,
        records,
        reviews,
    }
}

/// A review gate settlement comment: its attempt, verdict, reviewer and the
/// findings it closed by an authorized decision.
fn settlement(comment: &TaskComment) -> Option<Review> {
    let message = comment.message.trim_start();
    let rest = SETTLEMENT_LEADS
        .iter()
        .find_map(|lead| message.strip_prefix(lead))?;
    let (attempt_id, rest) = rest.split_once('`')?;
    let verdict = rest.strip_prefix(": verdict **")?.split_once("**")?.0;
    let verdict: ReviewVerdict = serde_json::from_value(Value::from(verdict)).ok()?;
    let mut reviewer = Runner::default();
    let mut disposed = Vec::new();
    for line in message.lines() {
        if let Some(rest) = line.strip_prefix(REVIEWER_LINE)
            && let Some((crew, rest)) = rest.split_once('`')
        {
            reviewer.crew = Some(crew.to_string());
            reviewer.model = rest
                .split_once(" / ")
                .and_then(|(_, model)| model.split_once(')'))
                .map(|(model, _)| model.trim().to_string())
                .filter(|model| !model.is_empty());
        } else if line.starts_with("- `") && line.contains(", disposed: ") {
            disposed.push(bounded(line.trim_start_matches("- ")));
        }
    }
    Some(Review {
        attempt_id: attempt_id.to_string(),
        at: comment.at,
        verdict,
        reviewer,
        disposed,
    })
}

/// The gate escalation of reviewer fixes that failed owner revalidation.
fn failed_fix(comment: &TaskComment) -> bool {
    comment.message.contains("## Review gate escalation")
        && comment.message.contains("Verdict: `reject`")
}
