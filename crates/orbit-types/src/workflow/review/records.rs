//! Stable required-check record ids across review report revisions
//! [ORB-14370].
//!
//! A required check an earlier revision of an attempt's report recorded stays
//! an obligation. Matching it by command text could not converge: reviewers
//! legitimately rename, rewrap and concretise commands between revisions, so
//! a check that ran and passed read as dropped. A record id names the check
//! instead. A later revision carries the id forward with whatever command and
//! outcome are now current, or retires it with a reason. The artifact store
//! applies this rule when a report is attached, while the reviewer can still
//! correct it, and settlement applies the same rule to the final report.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::{
    ReviewReport, ReviewReportHistory, ReviewValidation, ValidationOutcome, ValidationRole,
};
use crate::workflow::ReviewHistoryError;

/// An earlier revision's required record that a report deliberately no
/// longer carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetiredValidation {
    /// The retired record's id.
    pub id: String,
    /// Why the check no longer applies; a retirement without one does not
    /// account for the record.
    pub reason: String,
}

/// How a report fails to account for an earlier revision's required record
/// that carries an id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordGap {
    /// No record carries the id and no retirement names it.
    Omitted,
    /// Records carry the id only in a role that cannot account for a
    /// required check: a diagnostic, a negative control, or an exclusion of
    /// a check that ran.
    Reclassified(ValidationRole),
    /// A retirement names the id with an empty reason.
    RetirementUnexplained,
    /// A retirement names a record that failed. A failure is rerun under its
    /// id, or superseded by a passing rerun, never retired.
    FailureRetired,
}

impl RecordGap {
    /// What the report did with the record, completing "the report …".
    pub fn describe(self) -> String {
        match self {
            RecordGap::Omitted => "omits it".to_string(),
            RecordGap::Reclassified(role) => format!("reclassifies it as {}", role.as_str()),
            RecordGap::RetirementUnexplained => "retires it without a reason".to_string(),
            RecordGap::FailureRetired => "retires it although it failed".to_string(),
        }
    }
}

impl ReviewValidation {
    /// The record's non-empty, trimmed id.
    pub fn record_id(&self) -> Option<&str> {
        self.id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
    }

    /// The record's first non-empty deferral notice, when the run deferred
    /// a sandbox-confined path [ORB-14334].
    pub fn deferral(&self) -> Option<&str> {
        self.deferred
            .iter()
            .map(|notice| notice.trim())
            .find(|notice| !notice.is_empty())
    }

    /// What the run established about its check. A pass that deferred a
    /// sandbox-confined path never executed it, so it reads as `not_run`:
    /// it neither satisfies a required check nor replaces one [ORB-14334].
    pub fn executed_outcome(&self) -> ValidationOutcome {
        if self.outcome == ValidationOutcome::Passed && self.deferral().is_some() {
            ValidationOutcome::NotRun
        } else {
            self.outcome
        }
    }
}

/// How `records` and `retired` fail to account for the earlier required
/// record `id`, which was observed with `outcome`; `None` when they do.
///
/// A record carrying the id as `required` or `superseded` accounts for it
/// (the role rules then judge that record), as does `excluded` when the
/// earlier record never ran. Otherwise a retirement with a reason accounts
/// for any earlier record that did not fail.
pub fn record_gap(
    id: &str,
    outcome: ValidationOutcome,
    records: &[ReviewValidation],
    retired: &[RetiredValidation],
) -> Option<RecordGap> {
    let never_ran = matches!(
        outcome,
        ValidationOutcome::NotRun | ValidationOutcome::Denied
    );
    let mut carriers = records
        .iter()
        .filter(|record| record.record_id() == Some(id))
        .peekable();
    let first_role = carriers.peek().map(|record| record.role);
    if carriers.any(|record| match record.role {
        ValidationRole::Required | ValidationRole::Superseded => true,
        ValidationRole::Excluded => never_ran,
        ValidationRole::ExpectedFailure | ValidationRole::Diagnostic => false,
    }) {
        return None;
    }
    if let Some(retirement) = retired.iter().find(|retired| retired.id.trim() == id) {
        return if retirement.reason.trim().is_empty() {
            Some(RecordGap::RetirementUnexplained)
        } else if outcome == ValidationOutcome::Failed {
            Some(RecordGap::FailureRetired)
        } else {
            None
        };
    }
    Some(first_role.map_or(RecordGap::Omitted, RecordGap::Reclassified))
}

impl ReviewReportHistory {
    /// Refuse a newly submitted report whose required validation records lack
    /// stable ids, naming every such record and a free id for each so a
    /// reviewer still following id-less instructions can resubmit in one
    /// step. Stored reports without ids still parse and settle under the
    /// command-identity rules; this check applies at report attachment time.
    pub fn check_required_record_ids(report: &ReviewReport) -> Result<(), ReviewHistoryError> {
        let used = report
            .validation
            .iter()
            .filter_map(ReviewValidation::record_id)
            .collect::<BTreeSet<_>>();
        let mut free = (1..)
            .map(|n| format!("V{n}"))
            .filter(|id| !used.contains(id.as_str()));
        let missing = report
            .validation
            .iter()
            .filter(|record| {
                record.role == ValidationRole::Required && record.record_id().is_none()
            })
            .map(|record| (one_line(&record.command), free.next().unwrap_or_default()))
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(ReviewHistoryError::RecordIdsMissing { missing });
        }

        let mut records_by_id: BTreeMap<&str, Vec<&ReviewValidation>> = BTreeMap::new();
        for record in &report.validation {
            if let Some(id) = record.record_id() {
                records_by_id.entry(id).or_default().push(record);
            }
        }
        for (id, records) in records_by_id {
            let is_superseded_pair = records.len() == 2
                && records
                    .iter()
                    .filter(|record| record.role == ValidationRole::Required)
                    .count()
                    == 1
                && records
                    .iter()
                    .filter(|record| record.role == ValidationRole::Superseded)
                    .count()
                    == 1;
            if records.len() > 1 && !is_superseded_pair {
                return Err(ReviewHistoryError::RecordIdReused {
                    id: id.to_string(),
                    commands: records
                        .iter()
                        .map(|record| one_line(&record.command))
                        .collect(),
                });
            }
        }
        Ok(())
    }

    /// Refuse `report` when it drops a required record an earlier revision of
    /// its attempt filed under an id, naming that record and how to fix the
    /// report. Records written without ids are left to settlement's
    /// command-identity rules.
    pub fn check_record_continuity(&self, report: &ReviewReport) -> Result<(), ReviewHistoryError> {
        for revision in self.for_attempt(&report.attempt_id) {
            for earlier in &revision.validation {
                let Some(id) = earlier
                    .record_id()
                    .filter(|_| earlier.role == ValidationRole::Required)
                else {
                    continue;
                };
                let Some(gap) = record_gap(
                    id,
                    earlier.outcome,
                    &report.validation,
                    &report.retired_validation,
                ) else {
                    continue;
                };
                return Err(ReviewHistoryError::RecordDropped {
                    id: id.to_string(),
                    command: one_line(&earlier.command),
                    outcome: earlier.outcome,
                    attempt_id: report.attempt_id.clone(),
                    gap,
                });
            }
        }
        Ok(())
    }
}

fn one_line(command: &str) -> String {
    command.split_whitespace().collect::<Vec<_>>().join(" ")
}
