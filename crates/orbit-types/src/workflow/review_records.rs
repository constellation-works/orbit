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

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::review::{
    REVIEW_REPORT_ARTIFACT, ReviewReport, ReviewReportHistory, ReviewValidation, ValidationOutcome,
    ValidationRole,
};

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
    /// Refuse a newly submitted required validation record without its stable
    /// id. Existing stored reports still parse and settle under their legacy
    /// command-identity rules; this check applies at report attachment time.
    pub fn check_required_record_ids(report: &ReviewReport) -> Result<(), String> {
        for record in &report.validation {
            if record.role == ValidationRole::Required && record.record_id().is_none() {
                return Err(format!(
                    "{REVIEW_REPORT_ARTIFACT}: required validation record `{}` must have a stable, non-empty `id` (for example `V1`); add `\"id\": \"V1\"` and attach the report again",
                    record
                        .command
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                ));
            }
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
                let commands = records
                    .iter()
                    .map(|record| {
                        format!(
                            "`{}`",
                            record
                                .command
                                .split_whitespace()
                                .collect::<Vec<_>>()
                                .join(" ")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(format!(
                    "{REVIEW_REPORT_ARTIFACT}: validation record id `{id}` is used by multiple records ({commands}); give each check a distinct id. Only one `superseded` attempt and its `required` replacement may share an id; correct the report and attach it again"
                ));
            }
        }
        Ok(())
    }

    /// Refuse `report` when it drops a required record an earlier revision of
    /// its attempt filed under an id, naming that record and how to fix the
    /// report. Records written without ids are left to settlement's
    /// command-identity rules.
    pub fn check_record_continuity(&self, report: &ReviewReport) -> Result<(), String> {
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
                let retire = if earlier.outcome == ValidationOutcome::Failed {
                    "; a failed record cannot be retired, so rerun it".to_string()
                } else {
                    format!(
                        ", or list it in `retired_validation` as \
                         {{\"id\": \"{id}\", \"reason\": \"<why it no longer applies>\"}}"
                    )
                };
                return Err(format!(
                    "{REVIEW_REPORT_ARTIFACT}: required validation record `{id}` (`{}`) was \
                     recorded {} by an earlier revision of attempt {} and this report {}. Carry \
                     `\"id\": \"{id}\"` forward on a `required` record (or a `superseded` one \
                     with its passing replacement) holding the check's current command and \
                     outcome{retire}; then attach the report again",
                    earlier
                        .command
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" "),
                    earlier.outcome.as_str(),
                    report.attempt_id,
                    gap.describe(),
                ));
            }
        }
        Ok(())
    }
}
