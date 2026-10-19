//! The workspace scan behind the orchestrator scorecard: one pass over every
//! task an orchestrator is attributed to (envelope, history and comments),
//! plus the typed `regression_from` links that point at them, reduced to the
//! per-task facts the scorecard slices by window.
//!
//! Every count comes from a record Orbit wrote: history entries, comments the
//! review gate posts, relations and pull request references. Nothing reads a
//! self-report, and a task without a typed origin is never assigned one.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::task::{
    GITHUB_PR_EXTERNAL_REF_SYSTEM, RegressionEscape, RegressionNature, Task, TaskComment,
    TaskComplexity, TaskHistoryEntry, TaskOrigin, TaskRelationType, TaskStatus,
};

use crate::OrbitRuntime;
pub(super) use crate::application::agents::records::{Cause, Run};
use crate::application::agents::records::{
    block_cause, blocked_pr, bounded, cites_criteria, is_operator_requeue, label, landed,
    landing_pr, restart_cause, run_spans, spec_blocker_kind, spec_documents,
};
use crate::application::codebase::failed_claim_settlements;

/// One counted component of the scorecard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Component {
    /// An edit to the description or acceptance criteria after the first run started.
    SpecEdit,
    /// An agent blocker whose kind says a criterion or requirement is unmet.
    CriteriaBlock,
    /// A final-recovery escalation whose diagnosis cites the criteria.
    CriteriaEscalation,
    /// A typed regression against the task: nature `incomplete` or escape `spec_ambiguous`.
    SpecRegression,
    /// A review gate settlement with verdict `reject`.
    ReviewReject,
    /// A `[BLOCKED]` PR, or a delivery PR closed without merging.
    PrRework,
    /// An implementation run after the task's first.
    RepeatRun,
    /// A blocked task returned to the backlog by hand.
    OperatorRequeue,
}

/// One counted record behind a component: when, what it was, and for rework
/// its attributed cause.
#[derive(Debug, Clone)]
pub(super) struct Occurrence {
    pub(super) component: Component,
    pub(super) at: DateTime<Utc>,
    pub(super) detail: String,
    pub(super) cause: Option<Cause>,
}

#[derive(Debug, Clone)]
pub(super) struct TaskFacts {
    pub(super) id: String,
    pub(super) title: String,
    pub(super) orchestrator: String,
    pub(super) origin: Option<TaskOrigin>,
    pub(super) complexity: Option<TaskComplexity>,
    pub(super) status: TaskStatus,
    /// Places the task in a window.
    pub(super) created_at: DateTime<Utc>,
    /// Done, or archived after it was done.
    pub(super) landed: bool,
    /// Rejected or archived without ever being done.
    pub(super) dropped: bool,
    pub(super) runs: Vec<Run>,
    pub(super) occurrences: Vec<Occurrence>,
}

/// One workspace scan: the attributed tasks the scorecard slices by window.
#[derive(Debug)]
pub struct OrchestratorFacts {
    pub(super) computed_at: DateTime<Utc>,
    pub(super) tasks_scanned: usize,
    pub(super) tasks: Vec<TaskFacts>,
    /// When each task with no orchestrator attribution was created.
    pub(super) unattributed: Vec<DateTime<Utc>>,
}

/// Scan the workspace once. A task whose history or comments cannot be read
/// keeps its envelope facts rather than failing the whole view.
pub(super) fn scan(runtime: &OrbitRuntime) -> Result<OrchestratorFacts, OrbitError> {
    let computed_at = Utc::now();
    let tasks = runtime.list_task_metadata()?;
    let tasks_scanned = tasks.len();

    let mut regressions: BTreeMap<String, Vec<Occurrence>> = BTreeMap::new();
    for task in &tasks {
        for relation in &task.relations {
            if relation.relation_type != TaskRelationType::RegressionFrom {
                continue;
            }
            let Some(classification) = relation.regression else {
                continue;
            };
            if classification.nature != RegressionNature::Incomplete
                && classification.escape != RegressionEscape::SpecAmbiguous
            {
                continue;
            }
            regressions
                .entry(relation.target.to_string())
                .or_default()
                .push(Occurrence {
                    component: Component::SpecRegression,
                    at: task.created_at,
                    detail: format!(
                        "{} regressed it (nature {}, escape {})",
                        task.id,
                        label(&classification.nature),
                        label(&classification.escape),
                    ),
                    cause: None,
                });
        }
    }

    let settlements = failed_claim_settlements(runtime);
    let mut facts = Vec::new();
    let mut unattributed = Vec::new();
    for task in tasks {
        let Some(orchestrator) = task
            .orchestrator
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string)
        else {
            unattributed.push(task.created_at);
            continue;
        };
        let id = task.id.to_string();
        let history = runtime.get_task_history(&id).unwrap_or_else(|error| {
            tracing::warn!(task_id = %id, %error, "orchestrator scorecard: task history unreadable");
            Vec::new()
        });
        let comments = runtime.get_task_comments(&id).unwrap_or_else(|error| {
            tracing::warn!(task_id = %id, %error, "orchestrator scorecard: task comments unreadable");
            Vec::new()
        });
        let task_settlements = settlements.get(&id).map(Vec::as_slice).unwrap_or_default();
        let classify = |entry: &TaskHistoryEntry| block_cause(entry, task_settlements);
        let mut task_facts = task_facts(&task, orchestrator, &history, &comments, &classify);
        task_facts
            .occurrences
            .extend(regressions.remove(&id).unwrap_or_default());
        task_facts
            .occurrences
            .sort_by_key(|occurrence| occurrence.at);
        facts.push(task_facts);
    }

    Ok(OrchestratorFacts {
        computed_at,
        tasks_scanned,
        tasks: facts,
        unattributed,
    })
}

/// The facts of one attributed task. `classify` gives the cause a block or
/// run exit records.
fn task_facts(
    task: &Task,
    orchestrator: String,
    history: &[TaskHistoryEntry],
    comments: &[TaskComment],
    classify: &dyn Fn(&TaskHistoryEntry) -> Cause,
) -> TaskFacts {
    let runs = run_spans(history);
    let landed = landed(task.status, history);
    let dropped = matches!(task.status, TaskStatus::Archived | TaskStatus::Rejected) && !landed;
    let mut occurrences = Vec::new();

    // Spec defects recorded on the task's own history.
    let first_start = runs.first().map(|run| run.start);
    for entry in history {
        if let Some(first) = first_start
            && entry.at > first
            && let Some(documents) = spec_documents(entry)
        {
            occurrences.push(Occurrence {
                component: Component::SpecEdit,
                at: entry.at,
                detail: format!("{} edited by {}", documents.join(" and "), entry.by),
                cause: None,
            });
        }
        if let Some(kind) = spec_blocker_kind(entry) {
            occurrences.push(Occurrence {
                component: Component::CriteriaBlock,
                at: entry.at,
                detail: format!("agent blocker kind {kind}"),
                cause: None,
            });
        }
        if cites_criteria(entry) {
            occurrences.push(Occurrence {
                component: Component::CriteriaEscalation,
                at: entry.at,
                detail: bounded(entry.note.as_deref().unwrap_or_default()),
                cause: None,
            });
        }
    }

    // Review gate rejections, from the settlement comments the gate posts.
    let mut attempts = BTreeSet::new();
    for comment in comments {
        if let Some(detail) = review_reject(comment)
            && attempts.insert(detail.clone())
        {
            occurrences.push(Occurrence {
                component: Component::ReviewReject,
                at: comment.at,
                detail,
                cause: Some(Cause::Crew),
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
            occurrences.push(Occurrence {
                component: Component::PrRework,
                at: entry.at,
                detail: format!("[BLOCKED] PR #{number}"),
                cause: Some(classify(entry)),
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
            occurrences.push(Occurrence {
                component: Component::PrRework,
                at: settled_at,
                detail: format!("PR #{} closed without merging", reference.id),
                cause: Some(Cause::Other),
            });
        }
    }

    // Each run after the first, attributed to what ended the one before it.
    for (index, pair) in runs.windows(2).enumerate() {
        let [previous, run] = pair else { continue };
        occurrences.push(Occurrence {
            component: Component::RepeatRun,
            at: run.start,
            detail: format!("run {} of {}", index + 2, runs.len()),
            cause: Some(restart_cause(
                history,
                Some(previous.start),
                run.start,
                classify,
            )),
        });
    }

    // Blocked tasks returned to the backlog by hand.
    for entry in history {
        if !is_operator_requeue(entry) {
            continue;
        }
        let since = runs
            .iter()
            .rev()
            .find(|run| run.start < entry.at)
            .map(|run| run.start);
        occurrences.push(Occurrence {
            component: Component::OperatorRequeue,
            at: entry.at,
            detail: format!("{} by {}", entry.event, entry.by),
            cause: Some(restart_cause(history, since, entry.at, classify)),
        });
    }

    TaskFacts {
        id: task.id.to_string(),
        title: task.title.clone(),
        orchestrator,
        origin: task.origin,
        complexity: task.complexity,
        status: task.status,
        created_at: task.created_at,
        landed,
        dropped,
        runs,
        occurrences,
    }
}

/// The attempt a review gate settlement comment rejected, as
/// `review attempt <id> rejected`, or the gate escalation of reviewer fixes
/// that failed owner revalidation.
fn review_reject(comment: &TaskComment) -> Option<String> {
    let message = comment.message.trim_start();
    for lead in [
        "before-PR review settled attempt `",
        "before-landing review settled attempt `",
    ] {
        if let Some(rest) = message.strip_prefix(lead) {
            let (attempt, verdict) = rest.split_once('`')?;
            return verdict
                .starts_with(": verdict **reject**")
                .then(|| format!("review attempt {attempt} rejected"));
        }
    }
    (message.contains("## Review gate escalation") && message.contains("Verdict: `reject`")).then(
        || {
            format!(
                "reviewer fixes failed owner revalidation at {}",
                comment.at.to_rfc3339()
            )
        },
    )
}
