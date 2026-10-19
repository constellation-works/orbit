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
    ACCEPTANCE_CRITERIA_DOCUMENT, DESCRIPTION_DOCUMENT, GITHUB_PR_EXTERNAL_REF_SYSTEM,
    RegressionEscape, RegressionNature, TASK_FIELDS_UPDATED_EVENT, Task, TaskComment,
    TaskComplexity, TaskHistoryEntry, TaskOrigin, TaskRelationType, TaskStatus,
    task_fields_updated_documents,
};
use orbit_types::workflow::{TASK_BLOCKED_BY_AGENT_EVENT, task_blocked_by_agent_kind};
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::codebase::{
    BlockCause, actor_kind, classify_block, failed_claim_settlements,
};

/// Status events that start an implementation run: worktree setup, a claim's
/// pull, or a resumed run's readmission.
const RUN_START_EVENTS: &[&str] = &["started", "pulled_by", "resume_readmitted"];
/// Final recovery handing the task to a person.
const FINAL_RECOVERY_ESCALATED_EVENT: &str = "final_recovery_escalated";
/// `orbit task requeue` returning a blocked task to the backlog.
const OPERATOR_REQUEUED_EVENT: &str = "operator_requeued";
/// Status writes a person or agent session made by hand.
const MANUAL_STATUS_EVENTS: &[&str] = &["status_changed", "forced"];
/// The note lead of a failure handoff that published a `[BLOCKED]` PR.
const BLOCKED_PR_NOTE: &str = "failure handoff published PR #";
/// Words of an agent blocker kind that say the task itself is in question:
/// a criterion that cannot be met, a contradictory or ambiguous requirement.
const SPEC_BLOCKER_WORDS: &[&str] = &[
    "criterion",
    "criteria",
    "acceptance",
    "spec",
    "specification",
    "requirement",
    "requirements",
    "ambiguous",
    "ambiguity",
    "contradictory",
    "contradiction",
    "scope",
];
/// The longest detail an occurrence keeps from a history note.
const DETAIL_CHARS: usize = 200;

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

/// What a rework occurrence is attributed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Cause {
    /// The task's own specification: an edit to what it asks, a criteria
    /// blocker or escalation.
    Spec,
    /// The crew's work: a run failure or a review rejection.
    Crew,
    /// An infrastructure failure the run failure classification names.
    Infrastructure,
    /// Anything else: base conflicts, manual blocks, unclassified causes.
    Other,
}

impl Cause {
    pub(super) const ALL: [Cause; 4] = [
        Cause::Spec,
        Cause::Crew,
        Cause::Infrastructure,
        Cause::Other,
    ];

    pub(super) fn as_str(self) -> &'static str {
        match self {
            Cause::Spec => "spec",
            Cause::Crew => "crew",
            Cause::Infrastructure => "infrastructure",
            Cause::Other => "other",
        }
    }
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

/// One implementation run: from the entry that moved the task into
/// `in-progress` to the next status change, open while none followed.
#[derive(Debug, Clone, Copy)]
pub(super) struct Run {
    pub(super) start: DateTime<Utc>,
    pub(super) end: Option<DateTime<Utc>>,
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
    let landed = task.status == TaskStatus::Done
        || (task.status == TaskStatus::Archived
            && history
                .iter()
                .any(|entry| entry.to_status == Some(TaskStatus::Done)));
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
        if entry.event == TASK_BLOCKED_BY_AGENT_EVENT
            && let Some(kind) = spec_blocker_kind(entry)
        {
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
        let Some(number) = entry
            .note
            .as_deref()
            .and_then(|note| note.strip_prefix(BLOCKED_PR_NOTE))
            .map(leading_digits)
            .filter(|number| !number.is_empty())
        else {
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

/// Each implementation run: an entry into `in-progress` by a run-start event
/// up to the next status change. A start with no change since closes the
/// earlier run at the new start.
fn run_spans(history: &[TaskHistoryEntry]) -> Vec<Run> {
    let mut runs = Vec::new();
    let mut open: Option<DateTime<Utc>> = None;
    for entry in history {
        if entry.to_status == Some(TaskStatus::InProgress)
            && RUN_START_EVENTS.contains(&entry.event.as_str())
        {
            if let Some(start) = open.take() {
                runs.push(Run {
                    start,
                    end: Some(entry.at),
                });
            }
            open = Some(entry.at);
        } else if let Some(start) = open
            && entry
                .to_status
                .is_some_and(|status| status != TaskStatus::InProgress)
        {
            runs.push(Run {
                start,
                end: Some(entry.at),
            });
            open = None;
        }
    }
    if let Some(start) = open {
        runs.push(Run { start, end: None });
    }
    runs
}

/// The description and acceptance criteria an `updated` entry names.
fn spec_documents(entry: &TaskHistoryEntry) -> Option<Vec<&'static str>> {
    if entry.event != TASK_FIELDS_UPDATED_EVENT {
        return None;
    }
    let named = task_fields_updated_documents(entry.note.as_deref()?);
    let documents: Vec<&'static str> = [DESCRIPTION_DOCUMENT, ACCEPTANCE_CRITERIA_DOCUMENT]
        .into_iter()
        .filter(|document| named.contains(document))
        .collect();
    (!documents.is_empty()).then_some(documents)
}

/// The kind of an agent blocker whose words name the task's criteria or
/// requirements.
fn spec_blocker_kind(entry: &TaskHistoryEntry) -> Option<&str> {
    let kind = task_blocked_by_agent_kind(entry.note.as_deref()?)?;
    kind.split(['_', '.', ':', '-'])
        .any(|word| SPEC_BLOCKER_WORDS.contains(&word.to_ascii_lowercase().as_str()))
        .then_some(kind)
}

/// A final-recovery escalation whose diagnosis cites the acceptance criteria.
fn cites_criteria(entry: &TaskHistoryEntry) -> bool {
    entry.event == FINAL_RECOVERY_ESCALATED_EVENT
        && entry.note.as_deref().is_some_and(|note| {
            let note = note.to_ascii_lowercase();
            note.contains("criterion") || note.contains("criteria")
        })
}

/// Whether the entry is a spec signal: an edit to what the task asks, or a
/// block or escalation over its criteria.
fn is_spec_signal(entry: &TaskHistoryEntry) -> bool {
    spec_documents(entry).is_some()
        || (entry.event == TASK_BLOCKED_BY_AGENT_EVENT && spec_blocker_kind(entry).is_some())
        || cites_criteria(entry)
}

/// The cause a block or run exit records.
fn block_cause(
    entry: &TaskHistoryEntry,
    settlements: &[crate::application::codebase::SettledFailure],
) -> Cause {
    if is_spec_signal(entry) {
        return Cause::Spec;
    }
    match classify_block(entry, settlements) {
        BlockCause::Infra(_) => Cause::Infrastructure,
        // An agent blocker that does not name the criteria is an external
        // dependency or decision, not the crew's work.
        BlockCause::Run if entry.event == TASK_BLOCKED_BY_AGENT_EVENT => Cause::Other,
        BlockCause::Run => Cause::Crew,
        BlockCause::BaseConflict(_)
        | BlockCause::Manual(_)
        | BlockCause::NotFailure
        | BlockCause::Unclassified => Cause::Other,
    }
}

/// Why work restarted at `at`: specification when anything since `since`
/// changed or questioned what the task asks, else the cause of the latest
/// status change in that span other than a requeue by hand.
fn restart_cause(
    history: &[TaskHistoryEntry],
    since: Option<DateTime<Utc>>,
    at: DateTime<Utc>,
    classify: &dyn Fn(&TaskHistoryEntry) -> Cause,
) -> Cause {
    let span: Vec<&TaskHistoryEntry> = history
        .iter()
        .filter(|entry| since.is_none_or(|since| entry.at > since) && entry.at < at)
        .collect();
    if span.iter().any(|entry| is_spec_signal(entry)) {
        return Cause::Spec;
    }
    span.iter()
        .rev()
        .find(|entry| {
            entry
                .to_status
                .is_some_and(|status| status != TaskStatus::InProgress)
                && !is_operator_requeue(entry)
        })
        .map_or(Cause::Other, |entry| classify(entry))
}

/// `orbit task requeue`, or a status write by a person or agent session that
/// moved the task from `blocked` to `backlog`.
fn is_operator_requeue(entry: &TaskHistoryEntry) -> bool {
    if entry.event == OPERATOR_REQUEUED_EVENT {
        return true;
    }
    MANUAL_STATUS_EVENTS.contains(&entry.event.as_str())
        && entry.from_status == Some(TaskStatus::Blocked)
        && entry.to_status == Some(TaskStatus::Backlog)
        && matches!(actor_kind(&entry.by), "operator" | "agent")
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

/// The pull request a done task landed through, when its history names it:
/// the owner landing's delivery record, or the completion note.
fn landing_pr(history: &[TaskHistoryEntry]) -> Option<String> {
    history
        .iter()
        .rev()
        .filter(|entry| entry.to_status == Some(TaskStatus::Done))
        .find_map(|entry| {
            let note = entry.note.as_deref()?;
            if let Ok(record) = serde_json::from_str::<Value>(note)
                && let Some(number) = record.pointer("/delivery/pr_number")
            {
                return number
                    .as_str()
                    .map(str::to_string)
                    .or_else(|| number.as_u64().map(|number| number.to_string()));
            }
            let (_, rest) = note.split_once("pull request #")?;
            let number = leading_digits(rest);
            (!number.is_empty()).then(|| number.to_string())
        })
}

fn leading_digits(text: &str) -> &str {
    let end = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    &text[..end]
}

fn bounded(text: &str) -> String {
    let text = text.trim();
    match text.char_indices().nth(DETAIL_CHARS) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.to_string(),
    }
}

/// A closed enum's wire label.
fn label<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default()
}
