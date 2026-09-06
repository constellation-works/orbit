//! Deterministic validation of typed examination evidence against frozen input.

use super::digest;
use crate::AutomationError;
use chrono::{DateTime, Utc};
use orbit_types::workflow::automation::*;

/// Facts supplied by Core after task/run authority, provenance and object checks.
pub struct EvidenceFacts {
    pub bytes: Vec<u8>,
    pub reference: String,
    pub submitted_by: String,
    pub artifact_digest: String,
    pub authorized: bool,
    pub source_verified: bool,
}

pub fn validate(
    attempt: &BatchAttempt,
    facts: &EvidenceFacts,
    now: DateTime<Utc>,
) -> Result<AcceptedCoverage, AutomationError> {
    let invalid = |reason: &str| AutomationError::Evidence(reason.into());

    if !facts.authorized {
        return Err(invalid("unauthorized_submitter"));
    }

    if !facts.source_verified {
        return Err(invalid("source_unverifiable"));
    }

    if facts.bytes.len() > 1_048_576 || facts.bytes.is_empty() {
        return Err(invalid("invalid_size"));
    }

    let evidence: CoverageEvidence =
        serde_json::from_slice(&facts.bytes).map_err(|e| invalid(&format!("malformed: {e}")))?;

    let batch = &attempt.batch;
    let evidence_digest = digest(&facts.bytes);

    if evidence_digest != facts.artifact_digest
        || facts.reference.is_empty()
        || facts.submitted_by.is_empty()
    {
        return Err(invalid("provenance_mismatch"));
    }

    // The evidence must name the exact batch and attempt whose input it was frozen against.
    if evidence.schema_version != 1
        || evidence.batch_id != batch.id
        || evidence.consumer != batch.consumer
        || evidence.epoch != batch.epoch
        || evidence.input_digest != attempt.input_digest
        || attempt.action_id.as_deref() != Some(&evidence.action_id)
        || evidence.attempt != attempt.attempt
        || evidence.coverage != batch.coverage
    {
        return Err(invalid("batch_or_attempt_mismatch"));
    }

    if evidence.from_exclusive != batch.from_exclusive
        || evidence.through_inclusive != batch.through_inclusive
    {
        return Err(invalid("revision_mismatch"));
    }

    if !evidence.examination_complete
        || evidence.examined_commits != batch.commits
        || evidence.examined_deliveries
            != batch
                .deliveries
                .iter()
                .map(|d| d.key.clone())
                .collect::<Vec<_>>()
    {
        return Err(invalid("incomplete_examination"));
    }

    if evidence.checks.is_empty()
        || evidence.checks.iter().any(|check| {
            check.subject.trim().is_empty()
                || check.method.trim().is_empty()
                || check.observation.trim().is_empty()
        })
    {
        return Err(invalid("missing_checks"));
    }

    Ok(AcceptedCoverage {
        batch_id: batch.id.clone(),
        action_id: evidence.action_id,
        input_digest: attempt.input_digest.clone(),
        evidence_digest,
        evidence: facts.bytes.clone(),
        evidence_reference: facts.reference.clone(),
        submitted_by: facts.submitted_by.clone(),
        accepted_at: now,
    })
}
