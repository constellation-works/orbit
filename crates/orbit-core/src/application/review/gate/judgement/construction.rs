//! Reading the reviewer's reports into a judgement and checking the task meaning.

use std::collections::BTreeMap;

use orbit_automation::review::{combined_task_meaning_digest, task_meaning_digest};
use orbit_common::OrbitError;
use orbit_common::security::release::sha256_hex;
use orbit_types::task::TaskArtifact;
use orbit_types::workflow::{
    REVIEW_CONTRACT_VERSION, REVIEW_REPORT_ARTIFACT, REVIEW_REPORT_HISTORY_ARTIFACT,
    RetainedObligation, ReviewAttempt, ReviewCertificate, ReviewReport, ReviewReportHistory,
    ReviewReportRevision, ReviewValidation, ReviewVerdict, ValidationRole,
};

use super::super::super::automation_error;
use crate::OrbitRuntime;

use super::super::context::GateContext;

use super::Judgement;

impl Judgement {
    /// Read the reports the reviewer persisted for this attempt on any of
    /// the bundle's tasks and merge them. Reports from before the attempt
    /// started are ignored; none at all, or one that is unreadable, names
    /// another contract, or names another attempt, is an incomplete review,
    /// never a pass. Benign shape drift is accepted ([`ReviewReport::parse`]).
    pub(in crate::application::review::gate) fn from_report(
        runtime: &OrbitRuntime,
        context: &GateContext,
        attempt: &ReviewAttempt,
    ) -> Result<Self, OrbitError> {
        let incomplete = |reason: &str| Self {
            escalation: Some(reason.to_string()),
            host_refused: true,
            ..Self::empty(context)
        };
        let mut reports = Vec::new();
        let mut initial_only = true;
        let mut revisions = Vec::new();
        let mut stale = false;
        for task_id in &context.task_ids {
            let Some(artifact) = runtime.get_task_artifact(task_id, REVIEW_REPORT_ARTIFACT)? else {
                continue;
            };
            let history = runtime.get_task_artifact(task_id, REVIEW_REPORT_HISTORY_ARTIFACT)?;
            let initial = match retained_revisions(task_id, history, attempt, &artifact.content) {
                Ok((kept, initial)) => {
                    revisions.extend(kept);
                    initial
                }
                Err(reason) => return Ok(incomplete(&reason)),
            };
            let manifest = runtime.get_task_artifact_manifest(task_id)?;
            if manifest
                .iter()
                .find(|file| file.path == REVIEW_REPORT_ARTIFACT)
                .is_some_and(|file| file.created_at < attempt.started_at)
            {
                stale = true;
                continue;
            }
            let report = match ReviewReport::parse(&artifact.content) {
                Ok(report) => report,
                Err(error) => {
                    return Ok(incomplete(&format!(
                        "report_unreadable: the report on {task_id}: {error}"
                    )));
                }
            };
            if report.schema_version != REVIEW_CONTRACT_VERSION {
                return Ok(incomplete(&format!(
                    "report_contract_mismatch: the report on {task_id} has schema_version {} \
                     instead of {REVIEW_CONTRACT_VERSION}",
                    report.schema_version
                )));
            }
            if report.attempt_id != attempt.attempt_id {
                return Ok(incomplete(&format!(
                    "report_attempt_mismatch: the report on {task_id} names attempt {} but {} \
                     was admitted",
                    report.attempt_id, attempt.attempt_id
                )));
            }
            initial_only &= initial;
            reports.push(report);
        }
        let Some(report) = merge_reports(reports) else {
            return Ok(incomplete(if stale {
                "report_stale: review-report.json predates this attempt"
            } else {
                "report_missing: the reviewer persisted no review-report.json"
            }));
        };
        let retained_obligations = retained_obligations(revisions, &report.validation);
        Ok(Self {
            external_evidence: report.external_evidence,
            verdict: report.verdict,
            findings: report.findings,
            validation: report.validation,
            retained_obligations,
            retired_validation: report.retired_validation,
            escalation: report.escalation,
            summary: report.summary,
            initial_report_only: initial_only,
            ..Self::empty(context)
        })
    }

    /// The judgement a held review settled with, restated for a run that
    /// settles it without a reviewer once its owed evidence arrived. The
    /// held requirements are the named evidence; the held records, findings
    /// and verdict are otherwise the review's own.
    pub(in crate::application::review::gate) fn from_held_certificate(
        context: &GateContext,
        hold: &orbit_types::workflow::ReviewEvidenceHold,
        certificate: &ReviewCertificate,
    ) -> Self {
        Self {
            verdict: certificate.verdict,
            external_evidence: hold.requirements.clone(),
            findings: certificate.findings.clone(),
            validation: certificate.validation.clone(),
            retained_obligations: certificate.retained_obligations.clone(),
            retired_validation: certificate.retired_validation.clone(),
            escalation: certificate.escalation.clone(),
            host_evidence: certificate.host_evidence.clone(),
            host_overrides: certificate.host_overrides.clone(),
            ..Self::empty(context)
        }
    }

    /// The skeleton every constructor starts from: an incomplete verdict
    /// with no records, under the owner's admitted commands and the task's
    /// current meaning. Each constructor overrides what its source decides.
    fn empty(context: &GateContext) -> Self {
        let admission = context.admission.as_ref();
        Self {
            verdict: ReviewVerdict::Incomplete,
            external_evidence: Vec::new(),
            findings: Vec::new(),
            validation: Vec::new(),
            validation_complete: false,
            required_validation_commands: admission
                .and_then(|admission| admission.required_validation_commands.clone()),
            baseline_commands: admission
                .map(|admission| admission.baseline_commands.clone())
                .unwrap_or_default(),
            retained_obligations: Vec::new(),
            retired_validation: Vec::new(),
            escalation: None,
            summary: String::new(),
            task_meaning_digest: context.task_digests.1.clone(),
            selectors_widened: Vec::new(),
            evidence_carried: None,
            host_refused: false,
            host_evidence: Vec::new(),
            host_overrides: Vec::new(),
            initial_report_only: false,
        }
    }

    /// Task criteria, scope, or contract changes during the review
    /// invalidate it. Selectors may grow — the reviewer through the task API,
    /// or Orbit widening for a path it changed; anything else re-establishes
    /// review.
    pub(in crate::application::review::gate) fn check_task_meaning(
        &mut self,
        context: &GateContext,
        attempt: &ReviewAttempt,
        admitted_selectors: &BTreeMap<String, Vec<String>>,
    ) -> Result<(), OrbitError> {
        if self.task_meaning_digest == attempt.task_meaning_digest {
            return Ok(());
        }
        if !selectors_only_grew(context, attempt, admitted_selectors)? {
            self.downgrade(
                "task_meaning_changed: task criteria, plan, scope, or relations changed \
                 during the review",
            );
        }
        Ok(())
    }
}

/// One report for the bundle: the most severe verdict, every distinct
/// finding and validation record, and every distinct summary and escalation.
fn merge_reports(reports: Vec<ReviewReport>) -> Option<ReviewReport> {
    let mut reports = reports.into_iter();
    let mut merged = reports.next()?;
    for report in reports {
        for required in report.external_evidence {
            if !merged.external_evidence.contains(&required) {
                merged.external_evidence.push(required);
            }
        }
        if verdict_severity(report.verdict) > verdict_severity(merged.verdict) {
            merged.verdict = report.verdict;
        }
        for finding in report.findings {
            if !merged.findings.contains(&finding) {
                merged.findings.push(finding);
            }
        }
        for record in report.validation {
            if !merged.validation.contains(&record) {
                merged.validation.push(record);
            }
        }
        for retirement in report.retired_validation {
            if !merged.retired_validation.contains(&retirement) {
                merged.retired_validation.push(retirement);
            }
        }
        append_distinct(&mut merged.summary, &report.summary, "\n");
        if let Some(escalation) = report.escalation {
            let current = merged.escalation.get_or_insert_with(String::new);
            append_distinct(current, &escalation, "; ");
        }
    }
    Some(merged)
}

/// The report revisions the host retained on `task_id` for this attempt,
/// other than the current report itself, and whether the current report is
/// the only revision the attempt ever recorded. A history that cannot be
/// read is an incomplete review, never an empty one; a missing history
/// proves nothing about the revisions.
fn retained_revisions(
    task_id: &str,
    history: Option<TaskArtifact>,
    attempt: &ReviewAttempt,
    current: &[u8],
) -> Result<(Vec<ReviewReportRevision>, bool), String> {
    let Some(history) = history else {
        return Ok((Vec::new(), false));
    };
    let history = ReviewReportHistory::parse(&history.content).map_err(|error| {
        format!("report_history_unreadable: the report history on {task_id}: {error}")
    })?;
    let current_sha256 = sha256_hex(current);
    if history
        .for_attempt(&attempt.attempt_id)
        .find(|revision| revision.sha256 == current_sha256)
        .is_some_and(|revision| revision.record_id_contract_checked == Some(false))
    {
        let report = ReviewReport::parse(current)
            .map_err(|error| format!("report_unreadable: the report on {task_id}: {error}"))?;
        ReviewReportHistory::check_required_record_ids(&report).map_err(|error| {
            format!("report_record_ids_invalid: the post-session report on {task_id}: {error}")
        })?;
        history
            .check_record_continuity(&report)
            .map_err(|error| format!("report_record_ids_invalid: {error}"))?;
    }
    let own = history.for_attempt(&attempt.attempt_id).collect::<Vec<_>>();
    let initial = matches!(own.as_slice(), [only] if only.sha256 == current_sha256);
    let kept = own
        .into_iter()
        .filter(|revision| revision.sha256 != current_sha256)
        .cloned()
        .collect();
    Ok((kept, initial))
}

/// The required-check records of earlier revisions, oldest first, minus any
/// the final report repeats verbatim: those add no information.
fn retained_obligations(
    revisions: Vec<ReviewReportRevision>,
    final_records: &[ReviewValidation],
) -> Vec<RetainedObligation> {
    let mut obligations: Vec<RetainedObligation> = Vec::new();
    for revision in revisions {
        for validation in revision.validation {
            if validation.role != ValidationRole::Required
                || final_records.contains(&validation)
                || obligations.iter().any(|kept| kept.validation == validation)
            {
                continue;
            }
            obligations.push(RetainedObligation {
                report_sha256: revision.sha256.clone(),
                observed_at: revision.observed_at,
                validation,
            });
        }
    }
    obligations.sort_by_key(|obligation| obligation.observed_at);
    obligations
}

fn verdict_severity(verdict: ReviewVerdict) -> u8 {
    match verdict {
        ReviewVerdict::Accept => 0,
        ReviewVerdict::AcceptWithFixes => 1,
        ReviewVerdict::Reject => 2,
        ReviewVerdict::Incomplete => 3,
    }
}

fn append_distinct(current: &mut String, addition: &str, separator: &str) {
    let addition = addition.trim();
    if addition.is_empty() || current.split(separator).any(|part| part.trim() == addition) {
        return;
    }
    if !current.trim().is_empty() {
        current.push_str(separator);
    }
    current.push_str(addition);
}

/// Whether every task still means what was admitted except for selectors
/// the reviewer added through the task API: restoring the admitted
/// selectors must reproduce the admitted digest, and the current selectors
/// must contain every admitted one.
fn selectors_only_grew(
    context: &GateContext,
    attempt: &ReviewAttempt,
    admitted_selectors: &BTreeMap<String, Vec<String>>,
) -> Result<bool, OrbitError> {
    let mut digests = Vec::with_capacity(context.tasks.len());
    for task in &context.tasks {
        let Some(admitted) = admitted_selectors.get(task.id.as_str()) else {
            return Ok(false);
        };
        if admitted
            .iter()
            .any(|selector| !task.context_files.contains(selector))
        {
            return Ok(false);
        }
        let mut restored = task.clone();
        restored.context_files = admitted.clone();
        digests.push((
            task.id.to_string(),
            task_meaning_digest(&restored).map_err(automation_error)?,
        ));
    }
    let restored_digest = combined_task_meaning_digest(&digests).map_err(automation_error)?;
    Ok(restored_digest == attempt.task_meaning_digest)
}
