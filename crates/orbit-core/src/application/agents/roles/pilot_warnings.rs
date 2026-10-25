//! Pilot warnings: how each task-pilot assessment of a `proposed` task
//! settled, and the three pilot terms that grade it.
//!
//! An assessment is one task-pilot receipt: the audit comment the apply
//! boundary sealed with the assessment it persisted. It is held when that
//! assessment carries at least one finding that holds promotion (a
//! `blocked_by`, ADR conflict, utility, surface or validation-tool finding;
//! advisory ones never count), and clean otherwise. Nothing else is read
//! from it: how it settled comes only from task history (edits, status
//! transitions) and blocked-task triage markers.
//!
//! A held assessment is **confirmed** when, before the task leaves
//! `proposed`, a writer other than the pilot or `system` edits the
//! description or acceptance criteria, or the task is rejected or archived;
//! or when it is approved with neither that nor a bare-note field edit and
//! its delivery then blocks or the task is rejected. It is **dismissed** when
//! the unedited approval reaches `done` with no block. A clean assessment
//! whose task was promoted is a **missed scope block** when block triage
//! later records a decision on it, and settles clean when the task finishes
//! without one. Everything else is excluded: an approval after only
//! bare-note edits (a dependency fix and a tag change look the same in
//! history), an assessment superseded by a later one with no scope edit in
//! between, one made outside `proposed`, and one still pending.
//!
//! `blocked_by`, utility and surface findings are judged against the task's
//! current dependency edges, never against which prerequisites are done now: the
//! assessment-time done state is not stored, and a finished prerequisite must not
//! turn a historical hold clean.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use orbit_types::task::{
    TASK_FIELDS_UPDATED_EVENT, Task, TaskComment, TaskHistoryEntry, TaskStatus,
    task_fields_updated_documents,
};
use serde_json::{Map, Value, json};

use crate::application::agents::AgentFacts;
use crate::application::agents::attribution::row_key;
use crate::application::agents::records::spec_documents;
use crate::application::agents::score::{Denominator, Occurrence, Stat, Tally, number};
use crate::application::codebase::PilotRun;
use crate::application::task::{BLOCKED_TASK_TRIAGED_EVENT, warning_hold_findings};

/// The actor that writes task-pilot receipts.
const PILOT_ACTOR: &str = "task-pilot";
/// Orbit's own writes, which never settle a warning.
const SYSTEM_ACTOR: &str = "system";
/// The block-triage marker field of a decision proposal: the block needs the
/// operator to decide what the task asks.
const DECISION_DISPOSITION: &str = "disposition=decision";
/// Settled assessments a row needs before its warning terms count.
pub(super) const MIN_SETTLED: usize = 10;
/// The warning terms, each rated per 100 settled assessments.
pub(super) const CONFIRMED: &str = "confirmed_warnings";
pub(super) const DISMISSED: &str = "dismissed_warnings";
pub(super) const MISSED: &str = "missed_scope_blocks";
/// The unit the warning terms are rated per 100 of.
pub(super) const SETTLED_UNIT: &str = "settled assessments";

/// How an assessment settled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::application::agents) enum Outcome {
    Confirmed,
    Dismissed,
    /// Clean, promoted, then blocked on a decision.
    Missed,
    /// Clean, promoted and finished with no decision block.
    Clean,
    /// Neither credited nor charged, and why.
    Excluded(&'static str),
    /// Proposed, or its delivery not finished.
    Pending,
}

/// One task-pilot assessment and how it settled.
#[derive(Debug, Clone)]
pub(in crate::application::agents) struct Assessment {
    /// When the receipt was written.
    pub(in crate::application::agents) at: DateTime<Utc>,
    pub(in crate::application::agents) operation_id: String,
    /// The pilot run the receipt names, when it names one.
    pub(in crate::application::agents) run_id: Option<String>,
    /// The kind of each finding that held promotion; empty when clean.
    pub(in crate::application::agents) held: Vec<String>,
    pub(in crate::application::agents) outcome: Outcome,
    /// When the outcome settled and the record that settled it.
    pub(in crate::application::agents) settled: Option<(DateTime<Utc>, String)>,
    /// The pilot run the receipt is attributed to, set after the scan.
    pub(in crate::application::agents) run: Option<PilotRun>,
}

impl Assessment {
    fn key(&self) -> (String, String) {
        let run = self.run.as_ref();
        row_key(
            run.and_then(|run| run.crew.as_deref()),
            run.and_then(|run| run.model.as_deref()),
        )
    }

    fn run_id(&self) -> Option<String> {
        self.run
            .as_ref()
            .map(|run| run.run_id.clone())
            .or_else(|| self.run_id.clone())
    }
}

/// A task's assessments and its block-triage markers.
#[derive(Debug, Clone, Default)]
pub(in crate::application::agents) struct Warnings {
    pub(in crate::application::agents) assessments: Vec<Assessment>,
    /// When block triage recorded any disposition on the task.
    pub(in crate::application::agents) triaged: Vec<DateTime<Utc>>,
}

/// Settle every task-pilot assessment of `task`.
pub(in crate::application::agents) fn settle(
    task: &Task,
    history: &[TaskHistoryEntry],
    comments: &[TaskComment],
) -> Warnings {
    let dependencies: Vec<String> = task
        .dependencies()
        .iter()
        .map(ToString::to_string)
        .collect();
    // Comments are stored oldest first.
    let receipts: Vec<(DateTime<Utc>, &str, Value)> = comments
        .iter()
        .filter(|comment| comment.by == PILOT_ACTOR)
        .filter_map(|comment| {
            let receipt = comment.receipt()?;
            let body: Value = serde_json::from_str(receipt.body).ok()?;
            body.get("assessment")
                .is_some_and(Value::is_object)
                .then_some((comment.at, receipt.operation_id, body))
        })
        .collect();
    let assessments = receipts
        .iter()
        .enumerate()
        .map(|(index, (at, operation_id, body))| {
            let held: Vec<String> =
                warning_hold_findings(&body["assessment"], &dependencies, &|_| false)
                    .into_iter()
                    .map(|finding| finding.kind)
                    .collect();
            let next = receipts.get(index + 1).map(|(at, _, _)| *at);
            let (outcome, settled) = outcome(history, *at, next, !held.is_empty());
            Assessment {
                at: *at,
                operation_id: (*operation_id).to_string(),
                run_id: body["pilot_run_id"]
                    .as_str()
                    .map(str::trim)
                    .filter(|run_id| !run_id.is_empty())
                    .map(str::to_string),
                held,
                outcome,
                settled,
                run: None,
            }
        })
        .collect();
    Warnings {
        assessments,
        triaged: history
            .iter()
            .filter(|entry| entry.event == BLOCKED_TASK_TRIAGED_EVENT)
            .map(|entry| entry.at)
            .collect(),
    }
}

/// A write by someone other than the pilot or Orbit itself.
fn by_writer(entry: &TaskHistoryEntry) -> bool {
    entry.by != PILOT_ACTOR && entry.by != SYSTEM_ACTOR
}

/// A block-triage marker recording a decision proposal.
fn decision_triage(entry: &TaskHistoryEntry) -> bool {
    entry.event == BLOCKED_TASK_TRIAGED_EVENT
        && entry.note.as_deref().is_some_and(|note| {
            note.split_whitespace()
                .any(|field| field == DECISION_DISPOSITION)
        })
}

type Settled = (Outcome, Option<(DateTime<Utc>, String)>);

/// How the assessment written at `at` settled, given the next one at `next`.
fn outcome(
    history: &[TaskHistoryEntry],
    at: DateTime<Utc>,
    next: Option<DateTime<Utc>>,
    held: bool,
) -> Settled {
    let status = history
        .iter()
        .filter(|entry| entry.at < at)
        .filter_map(|entry| entry.to_status)
        .next_back();
    if status != Some(TaskStatus::Proposed) {
        return (Outcome::Excluded("not proposed when assessed"), None);
    }
    let settled =
        |outcome, entry: &TaskHistoryEntry, detail: String| (outcome, Some((entry.at, detail)));
    let mut later = history.iter().filter(|entry| entry.at >= at);
    let mut bare_edit = false;
    let mut promoted = None;
    for entry in later.by_ref() {
        if next.is_some_and(|next| entry.at >= next) {
            return (Outcome::Excluded("superseded by a later assessment"), None);
        }
        if entry.event == TASK_FIELDS_UPDATED_EVENT && by_writer(entry) {
            if let Some(documents) = spec_documents(entry) {
                if held {
                    return settled(
                        Outcome::Confirmed,
                        entry,
                        format!("{} edited by {}", documents.join(" and "), entry.by),
                    );
                }
            } else if task_fields_updated_documents(entry.note.as_deref().unwrap_or_default())
                .is_empty()
            {
                bare_edit = true;
            }
        }
        match entry.to_status {
            None | Some(TaskStatus::Proposed) => {}
            Some(status @ (TaskStatus::Rejected | TaskStatus::Archived)) => {
                return if held {
                    settled(
                        Outcome::Confirmed,
                        entry,
                        format!("{status} while held, by {}", entry.by),
                    )
                } else {
                    (Outcome::Excluded("dropped while proposed"), None)
                };
            }
            Some(_) => {
                promoted = Some(entry);
                break;
            }
        }
    }
    let Some(promotion) = promoted else {
        return (Outcome::Pending, None);
    };
    if held && bare_edit {
        return (
            Outcome::Excluded("approved after a field edit naming no document"),
            None,
        );
    }
    for entry in later {
        if held {
            match entry.to_status {
                Some(TaskStatus::Blocked) => {
                    return settled(
                        Outcome::Confirmed,
                        entry,
                        format!("approved unedited by {}, then blocked", promotion.by),
                    );
                }
                Some(TaskStatus::Rejected) => {
                    return settled(
                        Outcome::Confirmed,
                        entry,
                        format!("approved unedited by {}, then rejected", promotion.by),
                    );
                }
                Some(TaskStatus::Done) => {
                    return settled(
                        Outcome::Dismissed,
                        entry,
                        format!(
                            "approved unedited by {}, then done with no block",
                            promotion.by
                        ),
                    );
                }
                Some(TaskStatus::Archived) => {
                    return (Outcome::Excluded("archived after approval"), None);
                }
                _ => {}
            }
        } else if decision_triage(entry) {
            return settled(
                Outcome::Missed,
                entry,
                "assessed clean, then block triage proposed a decision".to_string(),
            );
        } else if entry.to_status.is_some_and(TaskStatus::is_terminal) {
            return settled(
                Outcome::Clean,
                entry,
                "assessed clean and finished".to_string(),
            );
        }
    }
    (Outcome::Pending, None)
}

/// One pilot row's warning tallies in the window.
#[derive(Default)]
pub(super) struct WarningRow<'a> {
    pub(super) tallies: BTreeMap<&'static str, Tally<'a>>,
    /// Assessments whose outcome settled in the window.
    settled: usize,
    /// Assessments written in the window that are excluded or pending.
    excluded: usize,
    /// Assessments written in the window, and those that held.
    written: usize,
    held: usize,
    confirmed: usize,
    dismissed: usize,
    missed: usize,
}

impl WarningRow<'_> {
    /// The warning terms' shared denominator.
    pub(super) fn denominators(&self) -> BTreeMap<&'static str, Denominator> {
        [CONFIRMED, DISMISSED, MISSED]
            .into_iter()
            .map(|key| {
                (
                    key,
                    Denominator {
                        units: self.settled,
                        min_support: MIN_SETTLED,
                    },
                )
            })
            .collect()
    }

    /// Shown, not scored: precision, hold rate and the settlement counts.
    pub(super) fn stats(&self) -> Vec<Stat> {
        let decided = self.confirmed + self.dismissed;
        vec![
            Stat {
                key: "warning_precision_percent",
                label: "Warning precision (%)",
                value: if decided == 0 {
                    Value::Null
                } else {
                    number(self.confirmed as f64 * 100.0 / decided as f64)
                },
            },
            Stat {
                key: "holds_per_100_assessments",
                label: "Holds per 100 assessments",
                value: if self.written == 0 {
                    Value::Null
                } else {
                    number(self.held as f64 * 100.0 / self.written as f64)
                },
            },
            Stat {
                key: "settled_assessments",
                label: "Settled assessments",
                value: json!(self.settled),
            },
            Stat {
                key: "excluded_assessments",
                label: "Excluded or pending assessments",
                value: json!(self.excluded),
            },
            Stat {
                key: "missed_scope_block_count",
                label: "Missed scope blocks",
                value: json!(self.missed),
            },
        ]
    }
}

/// Each pilot row's warning tallies: assessments that settled since `since`
/// count in the terms, and those written since then in the hold rate and
/// the excluded count.
pub(super) fn rows(
    facts: &AgentFacts,
    since: DateTime<Utc>,
) -> BTreeMap<(String, String), WarningRow<'_>> {
    let mut rows: BTreeMap<(String, String), WarningRow<'_>> = BTreeMap::new();
    for task in &facts.tasks {
        for assessment in &task.warnings.assessments {
            let written = assessment.at >= since;
            let settled = assessment.settled.as_ref().filter(|(at, _)| *at >= since);
            if !written && settled.is_none() {
                continue;
            }
            let row = rows.entry(assessment.key()).or_default();
            if written {
                row.written += 1;
                row.held += usize::from(!assessment.held.is_empty());
                if matches!(assessment.outcome, Outcome::Excluded(_) | Outcome::Pending) {
                    row.excluded += 1;
                }
            }
            let Some((at, detail)) = settled else {
                continue;
            };
            row.settled += 1;
            let key = match assessment.outcome {
                Outcome::Confirmed => {
                    row.confirmed += 1;
                    CONFIRMED
                }
                Outcome::Dismissed => {
                    row.dismissed += 1;
                    DISMISSED
                }
                Outcome::Missed => {
                    row.missed += 1;
                    MISSED
                }
                _ => continue,
            };
            let detail = if assessment.held.is_empty() {
                detail.clone()
            } else {
                format!("held on {}; {detail}", assessment.held.join(", "))
            };
            row.tallies.entry(key).or_default().add(
                task,
                1.0,
                Some(Occurrence {
                    at: *at,
                    detail,
                    run_id: assessment.run_id(),
                }),
            );
        }
    }
    rows
}

/// The pilots header: the warning terms' minimum support and whether the
/// missed-scope-block source ran in the window. Block triage writes the
/// markers it reads; with none in the window the term reads 0 because
/// nothing measured it, not because no clean assessment missed a block.
pub(super) fn header(facts: &AgentFacts, since: DateTime<Utc>) -> Map<String, Value> {
    let records = facts
        .tasks
        .iter()
        .flat_map(|task| &task.warnings.triaged)
        .filter(|at| **at >= since)
        .count();
    let available = records > 0;
    let mut header = Map::new();
    header.insert("warning_min_support".to_string(), json!(MIN_SETTLED));
    header.insert(
        "sources".to_string(),
        json!({
            MISSED: {
                "available": available,
                "triage_records": records,
                "detail": if available {
                    "Read from the blocked-task triage records in the window: a decision proposal on a block after a clean assessment is a miss."
                } else {
                    "Unavailable: no blocked-task triage record in the window, so missed scope blocks read 0 because nothing measured them, not because no clean assessment missed one. Enable the blocked-task triage routine, or triage blocks with orbit.task.block_triage, to measure them."
                },
            },
        }),
    );
    header
}
