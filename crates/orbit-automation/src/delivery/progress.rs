//! Append-only, provenance-bound completed examination across attempts.

use super::{digest, evidence};
use crate::AutomationError;
use chrono::{DateTime, Utc};
use orbit_types::workflow::automation::*;
use std::collections::{BTreeMap, BTreeSet};

fn invalid(reason: &str) -> AutomationError {
    AutomationError::Evidence(format!("progress_{reason}"))
}

/// Revalidate saved bytes and the exact examined portions before trusting a
/// current writer's references. Old artifacts retain their original identity.
pub fn validate_progress(
    attempt: &BatchAttempt,
    changed_paths: &BTreeMap<String, Vec<String>>,
    verified: &BTreeSet<String>,
) -> Result<BTreeMap<String, (String, DeliveryExamination)>, AutomationError> {
    let mut completed = BTreeMap::new();
    let mut digests = BTreeSet::new();
    let mut prior_attempt = 0;
    for snapshot in &attempt.progress {
        if !verified.contains(&snapshot.evidence_digest)
            || snapshot.evidence.len() > 1_048_576
            || digest(&snapshot.evidence) != snapshot.evidence_digest
            || !digests.insert(snapshot.evidence_digest.clone())
            || snapshot.reference.is_empty()
            || snapshot.submitted_by.is_empty()
            || snapshot.attempt == 0
            || snapshot.attempt > attempt.attempt
            || snapshot.attempt < prior_attempt
            || (snapshot.attempt == attempt.attempt
                && attempt.action_id.as_deref() != Some(&snapshot.action_id))
        {
            return Err(invalid("provenance_mismatch"));
        }
        prior_attempt = snapshot.attempt;
        let origin = BatchAttempt {
            action_id: Some(snapshot.action_id.clone()),
            attempt: snapshot.attempt,
            progress: vec![],
            ..attempt.clone()
        };
        let facts = evidence::EvidenceFacts {
            bytes: snapshot.evidence.clone(),
            reference: snapshot.reference.clone(),
            submitted_by: snapshot.submitted_by.clone(),
            artifact_digest: snapshot.evidence_digest.clone(),
            authorized: true,
            finalization_refusal: None,
            verified_progress: BTreeSet::new(),
            submission: snapshot.submission.clone(),
            source_verified: true,
            action_stopped: true,
            changed_paths: changed_paths.clone(),
        };
        // Authority was established at capture by Core, then frozen in the
        // fenced server checkpoint. This only checks that its bytes still bind
        // that original authorization to this exact batch and revisions.
        let prior = evidence::validate_submission(&origin, &facts)?;
        if prior.schema_version != COVERAGE_EVIDENCE_SCHEMA_VERSION
            || snapshot.deliveries.is_empty()
        {
            return Err(invalid("not_explicit_completion"));
        }
        validate_references(&prior, &completed)?;
        let mut unique = BTreeSet::new();
        for key in &snapshot.deliveries {
            if !unique.insert(key) || completed.contains_key(key) {
                return Err(invalid("duplicate_delivery"));
            }
            let entries = prior
                .delivery_examinations
                .iter()
                .filter(|entry| &entry.delivery == key)
                .collect::<Vec<_>>();
            let [entry] = entries.as_slice() else {
                return Err(invalid("missing_delivery"));
            };
            validate_completed_entry(&attempt.batch, entry, changed_paths)?;
            if entry.source_digest.is_some() {
                return Err(invalid("recursive_completion"));
            }
            completed.insert(
                key.clone(),
                (snapshot.evidence_digest.clone(), (*entry).clone()),
            );
        }
    }
    Ok(completed)
}

fn validate_completed_entry(
    batch: &CoverageBatch,
    entry: &DeliveryExamination,
    paths: &BTreeMap<String, Vec<String>>,
) -> Result<(), AutomationError> {
    if !entry.completed || !entry.skipped_paths.is_empty() {
        return Err(invalid("not_explicit_completion"));
    }
    let delivery = batch
        .deliveries
        .iter()
        .find(|delivery| delivery.key == entry.delivery)
        .ok_or_else(|| invalid("unknown_delivery"))?;
    let partial = CoverageBatch {
        deliveries: vec![delivery.clone()],
        ..batch.clone()
    };
    evidence::validate_deliveries(&partial, std::slice::from_ref(entry), paths).map_err(invalid)
}

fn validate_references(
    evidence: &CoverageEvidence,
    completed: &BTreeMap<String, (String, DeliveryExamination)>,
) -> Result<(), AutomationError> {
    for entry in &evidence.delivery_examinations {
        if let Some(source) = &entry.source_digest {
            let Some((digest, prior)) = completed.get(&entry.delivery) else {
                return Err(invalid("unknown_source"));
            };
            let mut local = entry.clone();
            local.source_digest = None;
            if source != digest || &local != prior {
                return Err(invalid("changed_examination"));
            }
        }
    }
    Ok(())
}

pub(super) fn validate_carried(
    attempt: &BatchAttempt,
    current: &CoverageEvidence,
    paths: &BTreeMap<String, Vec<String>>,
    verified: &BTreeSet<String>,
) -> Result<(), AutomationError> {
    let completed = validate_progress(attempt, paths, verified)?;
    validate_references(current, &completed)?;
    for snapshot in &attempt.progress {
        let prior: CoverageEvidence =
            serde_json::from_slice(&snapshot.evidence).map_err(|_| invalid("malformed"))?;
        if prior
            .findings
            .iter()
            .any(|finding| !current.findings.contains(finding))
            || prior
                .checks
                .iter()
                .any(|check| !current.checks.contains(check))
        {
            return Err(invalid("lost_findings_or_checks"));
        }
    }
    Ok(())
}

/// Capture only newly completed local portions; neither a provisional clean
/// verdict nor a batch-wide false assertion grants completion on its own.
pub(super) fn capture_completed_progress(
    attempt: &BatchAttempt,
    facts: &evidence::EvidenceFacts,
    now: DateTime<Utc>,
) -> Result<Option<CompletedReviewProgress>, AutomationError> {
    let current = evidence::validate_submission(attempt, facts)?;
    if current.schema_version != COVERAGE_EVIDENCE_SCHEMA_VERSION {
        return Ok(None);
    }
    validate_carried(
        attempt,
        &current,
        &facts.changed_paths,
        &facts.verified_progress,
    )?;
    if current.checks.is_empty()
        || current.checks.iter().any(|check| {
            check.subject.trim().is_empty()
                || check.method.trim().is_empty()
                || check.observation.trim().is_empty()
        })
    {
        return Err(invalid("missing_checks"));
    }
    let completed = validate_progress(attempt, &facts.changed_paths, &facts.verified_progress)?;
    let mut deliveries = Vec::new();
    let mut seen = BTreeSet::new();
    for entry in &current.delivery_examinations {
        if !seen.insert(&entry.delivery) {
            return Err(invalid("duplicate_delivery"));
        }
        if entry.completed
            && entry.source_digest.is_none()
            && !completed.contains_key(&entry.delivery)
        {
            validate_completed_entry(&attempt.batch, entry, &facts.changed_paths)?;
            deliveries.push(entry.delivery.clone());
        }
    }
    if deliveries.is_empty() {
        return Ok(None);
    }
    Ok(Some(CompletedReviewProgress {
        action_id: current.action_id,
        attempt: current.attempt,
        evidence_digest: facts.artifact_digest.clone(),
        evidence: facts.bytes.clone(),
        reference: facts.reference.clone(),
        submitted_by: facts.submitted_by.clone(),
        submission: facts.submission.clone(),
        captured_at: now,
        deliveries,
    }))
}

/// Preserve completed bytes before an operator reissues a settled action,
/// including when no scheduler reconciliation ran after it stopped.
pub fn retain_completed_progress(
    store: &dyn orbit_store::contracts::AutomationStoreBackend,
    state: &AutomationState,
    facts: &evidence::EvidenceFacts,
    now: DateTime<Utc>,
) -> Result<AutomationState, AutomationError> {
    let Some(active) = &state.active else {
        return Ok(state.clone());
    };
    let Some(snapshot) = capture_completed_progress(active, facts, now)? else {
        return Ok(state.clone());
    };
    let mut next = state.clone();
    if let Some(active) = &mut next.active {
        active.progress.push(snapshot);
    }
    crate::checkpoint::commit(store, state, next, None)
}
