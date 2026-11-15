//! Review-gate history notes [ORB-15834].
//!
//! `review_timeout_incomplete`, `review_awaiting_evidence`,
//! `review_evidence_received` and `review_evidence_findings` are written as
//! task-history note text and read back to decide which run and which
//! implementation tree they name.
//! [`ReviewGateHistoryNote`] is the only producer and parser of that text.
//! Rendered bytes stay the historical strings, including each producer's
//! separator, so rows already stored still parse. The history record stays a
//! note string.
//!
//! Failed reviewer-fix validation additionally records a versioned JSON note;
//! its display comment is read only for historical comment-only records.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::task::{self, TaskComment, TaskHistoryEntry};

/// History event published atomically with a failed reviewer-fix escalation.
pub const REVIEW_FIX_VALIDATION_FAILED_EVENT: &str = "review_fix_validation_failed";
/// Version of the JSON note on [`REVIEW_FIX_VALIDATION_FAILED_EVENT`].
pub const REVIEW_FIX_VALIDATION_FAILURE_SCHEMA_VERSION: u32 = 1;

/// Reviewer fixes that failed owner validation on the preserved candidate.
///
/// The attempt is provenance. Scoring still associates the failure with the
/// nearest earlier review, including for historical comment-only failures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedReviewFixValidationFailure {
    /// Version of this history note's contract.
    pub schema_version: u32,
    /// Delivery run that failed reviewer-fix validation.
    pub run_id: String,
    /// Admitted review attempt, when available.
    pub attempt_id: Option<String>,
    /// Preserved head whose validation failed.
    pub candidate_commit: String,
}

impl RecordedReviewFixValidationFailure {
    /// JSON history note with the same timestamp and author as its display comment.
    pub fn history_entry(
        &self,
        at: DateTime<Utc>,
        by: &str,
    ) -> Result<TaskHistoryEntry, serde_json::Error> {
        Ok(
            TaskHistoryEntry::new(at, by, REVIEW_FIX_VALIDATION_FAILED_EVENT)
                .with_note(Some(serde_json::to_string(self)?)),
        )
    }

    /// Decode this event's versioned note once; other events are not failures.
    #[must_use]
    pub fn from_entry(entry: &TaskHistoryEntry) -> Option<Self> {
        if entry.event != REVIEW_FIX_VALIDATION_FAILED_EVENT {
            return None;
        }
        serde_json::from_str::<Self>(entry.note.as_deref()?)
            .ok()
            .filter(|record| record.schema_version == REVIEW_FIX_VALIDATION_FAILURE_SCHEMA_VERSION)
    }
}

/// Failure timestamps for the task-facts boundary's nearest-earlier-review policy.
///
/// Typed history takes precedence over its paired display comment. Legacy
/// comments carry no required provenance, so this projection deliberately keeps
/// only the timestamp needed for scoring, rather than inventing missing fields.
#[must_use]
pub fn review_fix_validation_failure_times(
    history: &[TaskHistoryEntry],
    comments: &[TaskComment],
) -> Vec<DateTime<Utc>> {
    let recorded: BTreeSet<_> = history
        .iter()
        .filter_map(|entry| {
            RecordedReviewFixValidationFailure::from_entry(entry)
                .map(|_| (entry.at, entry.by.as_str()))
        })
        .collect();
    let mut failures: Vec<_> = recorded.iter().map(|(at, _)| *at).collect();
    failures.extend(
        comments
            .iter()
            .filter(|comment| !recorded.contains(&(comment.at, comment.by.as_str())))
            .filter(|comment| legacy_review_fix_validation_failure(comment))
            .map(|comment| comment.at),
    );
    failures.sort();
    failures
}

/// Sole compatibility decoder of the historical escalation prose.
fn legacy_review_fix_validation_failure(comment: &TaskComment) -> bool {
    comment.message.contains("## Review gate escalation")
        && comment.message.contains("Verdict: `reject`")
        // A settlement can quote escalation prose; settlement always wins.
        && task::recorded_review_settlements(&[], std::slice::from_ref(comment)).is_empty()
}

/// Fields a review-gate history note carries for the automation that reads it.
///
/// The note text also carries human prose (the failed step, the branch, why
/// delivery stopped). Those words are not decisions. They stay in the rendered
/// string and are not fields of this type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewGateHistoryNote {
    /// Run the note records.
    pub run_id: String,
    /// Candidate commit, when the note names one. Evidence-received notes do not.
    pub candidate: Option<String>,
    /// Candidate tree. Preservation notes name it, including rows written
    /// before `implementation_tree` existed.
    pub candidate_tree: Option<String>,
    /// Implementation tree a timeout requeue counts against. Absent on
    /// evidence notes and on preservation rows that predate the field.
    pub implementation_tree: Option<String>,
}

impl ReviewGateHistoryNote {
    /// Comma-separated preservation note the review-gate failure writer records.
    #[must_use]
    pub fn preservation_text(
        run_id: &str,
        failed_step: &str,
        candidate: &str,
        candidate_tree: &str,
        implementation_tree: &str,
        branch: &str,
        continuation: &str,
    ) -> String {
        format!(
            "before-PR review gate stopped delivery: run={run_id}, failed_step={failed_step}, \
             candidate={candidate}, candidate_tree={candidate_tree}, \
             implementation_tree={implementation_tree}, branch={branch}; no PR was opened; \
             {continuation}"
        )
    }

    /// Semicolon note for an evidence hold.
    ///
    /// `reason` is the owner-release summary appended after the fixed clause.
    /// Settlement writes the clause alone.
    #[must_use]
    pub fn awaiting_evidence_text(run_id: &str, candidate: &str, reason: Option<&str>) -> String {
        match reason {
            Some(reason) => format!(
                "run={run_id}; candidate={candidate}; awaiting named external checks; receipt \
                 queues a fresh review. {reason}"
            ),
            None => format!(
                "run={run_id}; candidate={candidate}; awaiting named external checks; receipt \
                 queues a fresh review"
            ),
        }
    }

    /// Semicolon note written when every named external check has arrived.
    ///
    /// `next` is the human clause before the trailing period.
    #[must_use]
    pub fn evidence_received_text(run_id: &str, next: &str) -> String {
        format!("run={run_id}; all named external checks arrived for the held candidate; {next}.")
    }

    /// [ORB-18660] Semicolon note written when an owner evidence run reported
    /// findings on the held candidate and queued it for repair.
    #[must_use]
    pub fn evidence_findings_text(run_id: &str, candidate: &str, results: usize) -> String {
        format!(
            "run={run_id}; candidate={candidate}; owner evidence reported {results} finding(s) \
             on the held candidate; queued to repair it."
        )
    }

    /// Parse a stored note. `None` when it names no run.
    #[must_use]
    pub fn from_text(note: &str) -> Option<Self> {
        let mut run_id = None;
        let mut candidate = None;
        let mut candidate_tree = None;
        let mut implementation_tree = None;
        for segment in split_note_fields(note) {
            let Some((key, value)) = field_body(segment).split_once('=') else {
                continue;
            };
            if !is_bare_key(key) {
                continue;
            }
            match key {
                "run" if run_id.is_none() => run_id = Some(value.to_string()),
                "candidate" if candidate.is_none() => candidate = Some(value.to_string()),
                "candidate_tree" if candidate_tree.is_none() => {
                    candidate_tree = Some(value.to_string());
                }
                "implementation_tree" if implementation_tree.is_none() => {
                    implementation_tree = Some(value.to_string());
                }
                _ => {}
            }
        }
        Some(Self {
            run_id: run_id.filter(|id| !id.is_empty())?,
            candidate,
            candidate_tree,
            implementation_tree,
        })
    }

    /// Tree a recorded timeout counted against.
    ///
    /// Rows written before the implementation tree was recorded carry only
    /// [`Self::candidate_tree`]. A present but empty `implementation_tree`
    /// stays empty and does not fall through to the candidate tree.
    #[must_use]
    pub fn recorded_tree(&self) -> Option<&str> {
        self.implementation_tree
            .as_deref()
            .or(self.candidate_tree.as_deref())
    }
}

fn is_bare_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
}

/// `key=value` body of a segment. A prefixed segment such as
/// `before-PR review gate stopped delivery: run=<id>` yields `run=<id>`.
fn field_body(segment: &str) -> &str {
    segment.rsplit_once(": ").map_or(segment, |(_, body)| body)
}

/// Split on the comma and semicolon separators the producers have used.
fn split_note_fields(note: &str) -> Vec<&str> {
    let mut fields = Vec::new();
    let bytes = note.as_bytes();
    let mut start = 0;
    let mut index = 0;
    while index + 1 < bytes.len() {
        let is_separator =
            (bytes[index] == b',' || bytes[index] == b';') && bytes[index + 1] == b' ';
        if is_separator {
            fields.push(&note[start..index]);
            start = index + 2;
            index = start;
            continue;
        }
        index += 1;
    }
    fields.push(&note[start..]);
    fields
}
