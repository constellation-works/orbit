//! Readers of the task records every scorecard counts from: implementation
//! run spans, spec signals, rework causes and the pull request a task landed
//! through. Shared by the agent scorecards' roles.
//!
//! Every reader takes a record Orbit wrote (history entries, their notes and
//! failure settlements). Nothing reads a self-report.

use chrono::{DateTime, Utc};
use orbit_types::task::{
    ACCEPTANCE_CRITERIA_DOCUMENT, DESCRIPTION_DOCUMENT, TASK_FIELDS_UPDATED_EVENT, TaskComplexity,
    TaskHistoryEntry, TaskStatus, task_fields_updated_documents,
};
use orbit_types::workflow::{TASK_BLOCKED_BY_AGENT_EVENT, task_blocked_by_agent_kind};
use serde_json::Value;

use crate::application::codebase::{BlockCause, SettledFailure, actor_kind, classify_block};

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
pub(in crate::application) const BLOCKED_PR_NOTE: &str = "failure handoff published PR #";
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

/// Complexity weights for weighted landed tasks; unassessed and unset weigh 1.
pub(in crate::application) const COMPLEXITY_WEIGHTS: &[(&str, i64)] = &[
    ("low", 1),
    ("medium", 2),
    ("hard", 4),
    ("xhard", 8),
    ("unassessed", 1),
];

/// A landed task's weight by complexity.
pub(in crate::application) fn complexity_weight(complexity: Option<TaskComplexity>) -> i64 {
    let name = complexity.map_or("unassessed", TaskComplexity::as_str);
    COMPLEXITY_WEIGHTS
        .iter()
        .find(|(key, _)| *key == name)
        .map_or(1, |(_, weight)| *weight)
}

/// What a rework occurrence is attributed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::application) enum Cause {
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
    pub(in crate::application) const ALL: [Cause; 4] = [
        Cause::Spec,
        Cause::Crew,
        Cause::Infrastructure,
        Cause::Other,
    ];
}

/// One implementation run: from the entry that moved the task into
/// `in-progress` to the next status change, open while none followed.
#[derive(Debug, Clone, Copy)]
pub(in crate::application) struct Run {
    pub(in crate::application) start: DateTime<Utc>,
    pub(in crate::application) end: Option<DateTime<Utc>>,
}

/// Whether the task landed: done, or archived after it was done.
pub(in crate::application) fn landed(status: TaskStatus, history: &[TaskHistoryEntry]) -> bool {
    status == TaskStatus::Done
        || (status == TaskStatus::Archived
            && history
                .iter()
                .any(|entry| entry.to_status == Some(TaskStatus::Done)))
}

/// Each implementation run: an entry into `in-progress` by a run-start event
/// up to the next status change. A start with no change since closes the
/// earlier run at the new start.
pub(in crate::application) fn run_spans(history: &[TaskHistoryEntry]) -> Vec<Run> {
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
pub(in crate::application) fn spec_documents(
    entry: &TaskHistoryEntry,
) -> Option<Vec<&'static str>> {
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
pub(in crate::application) fn spec_blocker_kind(entry: &TaskHistoryEntry) -> Option<&str> {
    if entry.event != TASK_BLOCKED_BY_AGENT_EVENT {
        return None;
    }
    let kind = task_blocked_by_agent_kind(entry.note.as_deref()?)?;
    kind.split(['_', '.', ':', '-'])
        .any(|word| SPEC_BLOCKER_WORDS.contains(&word.to_ascii_lowercase().as_str()))
        .then_some(kind)
}

/// A final-recovery escalation whose diagnosis cites the acceptance criteria.
pub(in crate::application) fn cites_criteria(entry: &TaskHistoryEntry) -> bool {
    entry.event == FINAL_RECOVERY_ESCALATED_EVENT
        && entry.note.as_deref().is_some_and(|note| {
            let note = note.to_ascii_lowercase();
            note.contains("criterion") || note.contains("criteria")
        })
}

/// Whether the entry is a spec signal: an edit to what the task asks, or a
/// block or escalation over its criteria.
fn is_spec_signal(entry: &TaskHistoryEntry) -> bool {
    spec_documents(entry).is_some() || spec_blocker_kind(entry).is_some() || cites_criteria(entry)
}

/// The cause a block or run exit records.
pub(in crate::application) fn block_cause(
    entry: &TaskHistoryEntry,
    settlements: &[SettledFailure],
) -> Cause {
    if is_spec_signal(entry) {
        return Cause::Spec;
    }
    match classify_block(entry, settlements) {
        BlockCause::Infra(_) => Cause::Infrastructure,
        // Review verdicts have their own charges. An escalation (including
        // an abandoned review) must not also charge crew rework.
        BlockCause::Run if entry.event == "review_gate_escalation" => Cause::Other,
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
pub(in crate::application) fn restart_cause(
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
pub(in crate::application) fn is_operator_requeue(entry: &TaskHistoryEntry) -> bool {
    if entry.event == OPERATOR_REQUEUED_EVENT {
        return true;
    }
    MANUAL_STATUS_EVENTS.contains(&entry.event.as_str())
        && entry.from_status == Some(TaskStatus::Blocked)
        && entry.to_status == Some(TaskStatus::Backlog)
        && matches!(actor_kind(&entry.by), "operator" | "agent")
}

/// The pull request number a `[BLOCKED]` failure handoff entry published.
pub(in crate::application) fn blocked_pr(entry: &TaskHistoryEntry) -> Option<&str> {
    entry
        .note
        .as_deref()
        .and_then(|note| note.strip_prefix(BLOCKED_PR_NOTE))
        .map(leading_digits)
        .filter(|number| !number.is_empty())
}

/// The pull request a done task landed through, when its history names it:
/// the owner landing's delivery record, or the completion note.
pub(in crate::application) fn landing_pr(history: &[TaskHistoryEntry]) -> Option<String> {
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

pub(in crate::application) fn leading_digits(text: &str) -> &str {
    let end = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    &text[..end]
}

/// `text` trimmed and cut to the longest detail an occurrence keeps.
pub(in crate::application) fn bounded(text: &str) -> String {
    let text = text.trim();
    match text.char_indices().nth(DETAIL_CHARS) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.to_string(),
    }
}

/// A closed enum's wire label.
pub(in crate::application) fn label<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default()
}
