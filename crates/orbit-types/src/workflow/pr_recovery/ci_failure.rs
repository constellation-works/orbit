//! Required-check failures on a candidate head, recorded with their cause.
//!
//! The landing records each failed required check it classifies as one
//! [`CiFailureRecord`] task-history event, and every later change of class as
//! a [`CiFailureReclassification`] event, so the class's audit trail is the
//! task's own history. [`ci_failure_ledger`] folds those events back into one
//! entry per head and check.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::task::TaskHistoryEntry;

/// One required-check failure on a candidate head, first observation.
pub const CI_FAILURE_RECORDED_EVENT: &str = "ci_failure_recorded";
/// A later change of a recorded failure's class, with its evidence.
pub const CI_FAILURE_RECLASSIFIED_EVENT: &str = "ci_failure_reclassified";
/// The record and reclassification schema version.
pub const CI_FAILURE_SCHEMA_VERSION: u32 = 1;

/// Who caused a required-check failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CiFailureClass {
    /// The head's own change: the same head failed the same check again on a
    /// rerun, or the failure cites a file the head changed.
    Code,
    /// The same head passed the same check on a rerun with no new commit.
    Flaky,
    /// A declared infrastructure signature (or the runner or network) failed,
    /// or the base branch fails the same check.
    Infrastructure,
    /// None of the above could be established.
    Unknown,
}

impl CiFailureClass {
    /// Every class, in display order.
    pub const ALL: [Self; 4] = [Self::Code, Self::Flaky, Self::Infrastructure, Self::Unknown];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Code => "code",
            Self::Flaky => "flaky",
            Self::Infrastructure => "infrastructure",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|class| class.as_str() == raw)
    }

    /// Only an `unknown` class is upgraded by later evidence; an established
    /// class is never rewritten.
    pub fn may_become(self, to: Self) -> bool {
        self == Self::Unknown && to != Self::Unknown
    }
}

/// One failed required check on one candidate head.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CiFailureRecord {
    pub schema_version: u32,
    pub task_id: String,
    pub head_sha: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_number: Option<u64>,
    /// The run that produced the head (the handed-off candidate's run).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub producing_run_id: Option<String>,
    /// `workflow / job`, or the check name outside a workflow.
    pub check: String,
    /// The failed step's error signature (`log_signature`).
    pub signature: String,
    pub class: CiFailureClass,
    /// The landing's own classification the class was derived from.
    pub landing_class: String,
    /// Why the class was chosen, citing the evidence.
    pub evidence: String,
}

/// A recorded failure's class changed because later evidence arrived.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CiFailureReclassification {
    pub schema_version: u32,
    pub head_sha: String,
    pub check: String,
    pub from: CiFailureClass,
    pub to: CiFailureClass,
    pub evidence: String,
}

/// One audited class change in a [`CiFailureLedgerEntry`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CiFailureClassChange {
    pub at: DateTime<Utc>,
    pub from: CiFailureClass,
    pub to: CiFailureClass,
    pub evidence: String,
}

/// A recorded failure with its current class and every change to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CiFailureLedgerEntry {
    pub recorded_at: DateTime<Utc>,
    pub record: CiFailureRecord,
    /// The current class: the record's, after every accepted change.
    pub class: CiFailureClass,
    pub changes: Vec<CiFailureClassChange>,
}

impl CiFailureLedgerEntry {
    /// Whether this entry is the failure of `check` on `head`.
    pub fn is(&self, head: &str, check: &str) -> bool {
        self.record.head_sha == head && self.record.check == check
    }
}

/// Fold one task's history into one entry per recorded head and check, in
/// recording order. The first record of a head and check wins; a
/// reclassification applies only when it starts from the entry's current
/// class and [`CiFailureClass::may_become`] allows it. Unreadable or other-
/// version notes are skipped.
pub fn ci_failure_ledger(history: &[TaskHistoryEntry]) -> Vec<CiFailureLedgerEntry> {
    let mut ledger: Vec<CiFailureLedgerEntry> = Vec::new();
    for entry in history {
        let Some(note) = entry.note.as_deref() else {
            continue;
        };
        if entry.event == CI_FAILURE_RECORDED_EVENT {
            let Ok(record) = serde_json::from_str::<CiFailureRecord>(note) else {
                continue;
            };
            if record.schema_version != CI_FAILURE_SCHEMA_VERSION
                || ledger
                    .iter()
                    .any(|known| known.is(&record.head_sha, &record.check))
            {
                continue;
            }
            ledger.push(CiFailureLedgerEntry {
                recorded_at: entry.at,
                class: record.class,
                record,
                changes: Vec::new(),
            });
        } else if entry.event == CI_FAILURE_RECLASSIFIED_EVENT {
            let Ok(change) = serde_json::from_str::<CiFailureReclassification>(note) else {
                continue;
            };
            if change.schema_version != CI_FAILURE_SCHEMA_VERSION {
                continue;
            }
            if let Some(known) = ledger
                .iter_mut()
                .find(|known| known.is(&change.head_sha, &change.check))
                && known.class == change.from
                && change.from.may_become(change.to)
            {
                known.class = change.to;
                known.changes.push(CiFailureClassChange {
                    at: entry.at,
                    from: change.from,
                    to: change.to,
                    evidence: change.evidence,
                });
            }
        }
    }
    ledger
}
