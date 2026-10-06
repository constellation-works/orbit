//! Hold external checks without accepting a candidate or blocking for repairs.

use orbit_automation::review::{
    ValidationContext, combined_task_meaning_digest, task_meaning_digest, validation_evidence,
};
use orbit_common::OrbitError;
use orbit_types::record::OrbitEvent;
use orbit_types::task::{Task, TaskStatus};
use orbit_types::workflow::{
    FindingDisposition, REVIEW_EVIDENCE_HOLD_ARTIFACT, REVIEW_GATE_ARTIFACT,
    REVIEW_MANIFEST_ARTIFACT, REVIEW_REPORT_ARTIFACT, REVIEW_REPORT_HISTORY_ARTIFACT,
    ReviewCertificate, ReviewEvidenceHold, ReviewEvidenceRequirement, ReviewExternalEvidence,
    ValidationOutcome, ValidationRole,
};

use crate::OrbitRuntime;
use crate::application::task::TaskRecordUpdateParams;

/// Only named, unavailable external checks may hold a review. Failed checks,
/// open defects, malformed roles and dropped obligations remain escalations.
pub(super) fn evidence_only(
    certificate: &ReviewCertificate,
    requirements: &[ReviewEvidenceRequirement],
) -> bool {
    if certificate.verdict.passed()
        || certificate.task_ids.len() != 1
        || requirements.is_empty()
        || certificate
            .findings
            .iter()
            .any(|finding| finding.disposition == FindingDisposition::Open)
    {
        return false;
    }
    let mut validation = certificate.validation.clone();
    let mut seen = std::collections::BTreeSet::new();
    for required in requirements {
        if required.name.trim().is_empty()
            || required.command.trim().is_empty()
            || matches!(
                required.artifact.as_str(),
                REVIEW_EVIDENCE_HOLD_ARTIFACT
                    | REVIEW_GATE_ARTIFACT
                    | REVIEW_MANIFEST_ARTIFACT
                    | REVIEW_REPORT_ARTIFACT
                    | REVIEW_REPORT_HISTORY_ARTIFACT
            )
            || orbit_types::task::validate_relative_artifact_path(&required.artifact).is_err()
            || !seen.insert(&required.artifact)
        {
            return false;
        }
        let mut matched = false;
        for record in &mut validation {
            if record.command == required.command && record.role == ValidationRole::Required {
                if !matches!(
                    record.outcome,
                    ValidationOutcome::NotRun | ValidationOutcome::Denied
                ) {
                    return false;
                }
                record.outcome = ValidationOutcome::Passed;
                matched = true;
            }
        }
        if !matched {
            return false;
        }
    }
    validation_evidence(
        &validation,
        &ValidationContext {
            scope: &certificate.validation_scope,
            obligations: &certificate.retained_obligations,
            required_validation_commands: certificate.required_validation_commands.as_deref(),
        },
    )
    .is_ok()
}

pub(crate) fn evidence_hold(
    runtime: &OrbitRuntime,
    task_id: &str,
) -> Result<Option<ReviewEvidenceHold>, OrbitError> {
    runtime
        .get_task_artifact(task_id, REVIEW_EVIDENCE_HOLD_ARTIFACT)?
        .map(|artifact| {
            serde_json::from_slice::<ReviewEvidenceHold>(&artifact.content).map_err(|error| {
                OrbitError::Execution(format!("review evidence hold is unreadable: {error}"))
            })
        })
        .transpose()
}

/// Re-read evidence rather than trusting a tag, task comment or summary.
pub(crate) fn evidence_ready(
    runtime: &OrbitRuntime,
    task_id: &str,
    hold: &ReviewEvidenceHold,
) -> Result<bool, OrbitError> {
    if hold.schema_version != 1 || hold.requirements.is_empty() {
        return Ok(false);
    }
    for required in &hold.requirements {
        let Some(artifact) = runtime.get_task_artifact(task_id, &required.artifact)? else {
            return Ok(false);
        };
        let Ok(evidence) = serde_json::from_slice::<ReviewExternalEvidence>(&artifact.content)
        else {
            return Ok(false);
        };
        if evidence.schema_version != 1
            || evidence.attempt_id != hold.attempt_id
            || evidence.candidate != hold.candidate
            || evidence.kind != required.kind
            || evidence.name != required.name
            || evidence.command != required.command
            || evidence.outcome != ValidationOutcome::Passed
            || evidence.log_artifact == required.artifact
            || evidence.log_artifact == REVIEW_EVIDENCE_HOLD_ARTIFACT
            || orbit_types::task::validate_relative_artifact_path(&evidence.log_artifact).is_err()
        {
            return Ok(false);
        }
        if runtime
            .get_task_artifact(task_id, &evidence.log_artifact)?
            .is_none_or(|log| log.content.is_empty())
        {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Whether `hold` is still the in-progress task's latest delivery decision:
/// the task is linked to the held run, its meaning is unchanged, and no later
/// status decision or review superseded the hold. A stale artifact left
/// behind after an operator changed the task or a later review failed is
/// never current.
pub(crate) fn hold_is_current(
    runtime: &OrbitRuntime,
    task: &Task,
    hold: &ReviewEvidenceHold,
) -> Result<bool, OrbitError> {
    if task.status != TaskStatus::InProgress
        || hold.schema_version != 1
        || task.job_run_id.as_deref() != Some(&hold.run_id)
        || combined_task_meaning_digest(&[(
            task.id.to_string(),
            task_meaning_digest(task).map_err(super::automation_error)?,
        )])
        .map_err(super::automation_error)?
            != hold.task_meaning_digest
    {
        return Ok(false);
    }
    let history = runtime.get_task_history(&task.id)?;
    if history.last().is_some_and(|entry| {
        matches!(
            entry.to_status,
            Some(
                TaskStatus::Blocked
                    | TaskStatus::Done
                    | TaskStatus::Archived
                    | TaskStatus::Rejected
            )
        )
    }) {
        return Ok(false);
    }
    Ok(history
        .iter()
        .rev()
        .find(|entry| entry.to_status.is_some() || entry.event == "review_awaiting_evidence")
        .is_some_and(|entry| entry.event == "review_awaiting_evidence"))
}

/// Called under the task write lock after an artifact update. All requirements
/// must match the held candidate and unchanged task meaning before requeueing.
/// This schedules another review; it never converts incomplete into accept.
pub(crate) fn resume_evidence_hold(
    runtime: &OrbitRuntime,
    task_id: &str,
) -> Result<(), OrbitError> {
    let task = runtime.get_task(task_id)?;
    if task.status != TaskStatus::InProgress {
        return Ok(());
    }
    let Some(hold) = evidence_hold(runtime, task_id)? else {
        return Ok(());
    };
    let Some(artifact) = runtime.get_task_artifact(task_id, REVIEW_GATE_ARTIFACT)? else {
        return Ok(());
    };
    let Ok(certificate) = serde_json::from_slice::<ReviewCertificate>(&artifact.content) else {
        return Ok(());
    };
    if certificate.attempt_id != hold.attempt_id
        || certificate.final_candidate != hold.candidate
        || certificate.task_meaning_digest != hold.task_meaning_digest
        || !evidence_only(&certificate, &hold.requirements)
        || !hold_is_current(runtime, &task, &hold)?
        || !evidence_ready(runtime, task_id, &hold)?
    {
        return Ok(());
    }
    runtime.with_mutation(|| {
        let updated = runtime.stores().task_records().update(task_id, TaskRecordUpdateParams {
            actor: "system".into(),
            status: Some(TaskStatus::Backlog),
            expected_status: Some(vec![TaskStatus::InProgress]),
            status_event: Some("review_evidence_received".into()),
            status_note: Some(format!("run={}; all named external checks arrived for the held candidate; queued for fresh review.", hold.run_id)),
            ..Default::default()
        })?;
        Ok((updated, OrbitEvent::TaskUpdated { id: task_id.into() }))
    })?;
    Ok(())
}
