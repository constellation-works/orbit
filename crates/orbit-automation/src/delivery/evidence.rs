//! Deterministic validation of typed examination evidence against frozen input.

use super::digest;
use crate::AutomationError;
use chrono::{DateTime, Utc};
use orbit_types::workflow::automation::*;
use std::collections::{BTreeMap, BTreeSet};

/// Shortest rationale settlement accepts for one delivery's verdict.
pub const MIN_RATIONALE_CHARS: usize = 40;

/// Facts supplied by Core after task/run authority, provenance and object checks.
pub struct EvidenceFacts {
    pub bytes: Vec<u8>,
    pub reference: String,
    pub submitted_by: String,
    pub artifact_digest: String,
    pub authorized: bool,
    pub source_verified: bool,
    /// Core proved the action stopped (its task is terminal, its run ended),
    /// so these bytes are final: invalid evidence settles the attempt rather
    /// than waiting for a resubmission that can no longer arrive.
    pub action_stopped: bool,
    /// Every path the `before..after` diff of each frozen delivery changes,
    /// keyed by delivery key, as Core read it from the verified source.
    pub changed_paths: BTreeMap<String, Vec<String>>,
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
    // Version 1 named commits only, which a reviewer can produce without
    // reading anything; receipts accepted under it stay settled.
    if evidence.schema_version != COVERAGE_EVIDENCE_SCHEMA_VERSION {
        return Err(invalid("unsupported_schema_version"));
    }

    if evidence.batch_id != batch.id
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

    validate_deliveries(batch, &evidence.delivery_examinations, &facts.changed_paths)
        .map_err(invalid)?;

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

/// Each frozen delivery needs exactly one examination whose examined and
/// skipped paths together are its changed paths, with a typed verdict and a
/// rationale of its own.
fn validate_deliveries(
    batch: &CoverageBatch,
    examinations: &[DeliveryExamination],
    changed_paths: &BTreeMap<String, Vec<String>>,
) -> Result<(), &'static str> {
    let mut by_key = BTreeMap::new();
    for examination in examinations {
        if by_key
            .insert(examination.delivery.as_str(), examination)
            .is_some()
        {
            return Err("duplicate_delivery_examination");
        }
    }
    if by_key.len() != batch.deliveries.len() {
        return Err("missing_delivery_examination");
    }

    let mut rationales = BTreeSet::new();
    for delivery in &batch.deliveries {
        let examination = by_key
            .get(delivery.key.as_str())
            .ok_or("missing_delivery_examination")?;
        let changed = changed_paths
            .get(&delivery.key)
            .ok_or("changed_paths_unavailable")?;

        let mut covered = BTreeSet::new();
        let skipped = examination.skipped_paths.iter().map(|skipped| {
            (!skipped.reason.trim().is_empty())
                .then_some(skipped.path.as_str())
                .ok_or("skipped_path_without_reason")
        });
        for path in examination
            .examined_paths
            .iter()
            .map(|path| Ok(path.as_str()))
            .chain(skipped)
        {
            if !covered.insert(path?) {
                return Err("examined_paths_mismatch");
            }
        }
        if covered != changed.iter().map(String::as_str).collect() {
            return Err("examined_paths_mismatch");
        }

        if let DeliveryVerdict::Findings(ids) = &examination.verdict
            && (ids.is_empty() || ids.iter().any(|id| id.trim().is_empty()))
        {
            return Err("invalid_verdict");
        }

        let rationale = examination.rationale.trim();
        if rationale.chars().count() < MIN_RATIONALE_CHARS || !rationales.insert(rationale) {
            return Err("trivial_rationale");
        }
    }
    Ok(())
}
