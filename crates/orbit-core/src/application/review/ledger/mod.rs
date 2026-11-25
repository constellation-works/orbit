//! Transactional review decisions over persisted evidence.
mod attempts;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::Tx;
use orbit_store::contracts::{
    ReviewInvocationRecord, ReviewRelease, ReviewReserveRequest, ReviewResetRequest,
    ReviewSettlement,
};
use orbit_types::workflow::{
    ReviewAttemptState, ReviewCertificate, ReviewLedger, ReviewReconciliation, ReviewReservation,
    ReviewResetDecision,
};

use attempts::{
    record_invocation as apply_invocation, release_attempt, reserve_new, reserve_new_in,
    review_spent, settle_attempt,
};
pub fn review_reset(
    tx: &Tx<'_>,
    workspace_id: &str,
    request: &ReviewResetRequest<'_>,
) -> Result<ReviewLedger, OrbitError> {
    if request.reason.trim().is_empty() || request.actor.trim().is_empty() {
        return Err(OrbitError::InvalidInput(
            "review reset requires a non-empty reason and actor".into(),
        ));
    }
    let mut ledger = tx
        .review()
        .ledger(workspace_id, request.lineage_key)?
        .ok_or_else(|| {
            OrbitError::InvalidInput("review reset: selected lineage has no ledger".into())
        })?;
    if !ledger.task_ids.iter().any(|id| id == request.task_id) {
        return Err(OrbitError::InvalidInput(
            "review reset: selected lineage does not contain this task".into(),
        ));
    }
    let revision = ledger.revision;
    if let Some(open) = ledger.open_attempt().cloned() {
        release_attempt(&mut ledger, &open.attempt_id, request.now, request.now);
    }
    let budget = request.budget.unwrap_or(ledger.budget);
    if budget.minutes == 0 {
        return Err(OrbitError::InvalidInput(
            "review reset requires a usable review budget".into(),
        ));
    }
    ledger.decisions.push(ReviewResetDecision {
        after_attempt_index: ledger.attempts.last().map_or(0, |a| a.index),
        reason: request.reason.trim().to_string(),
        actor: request.actor.to_string(),
        recorded_at: request.now,
        previous_budget: ledger.budget,
        previous_consumption: ledger.consumed(),
        budget,
    });
    ledger.budget = budget;
    ledger.consumed_seconds = 0;
    tx.review()
        .put_ledger(workspace_id, Some(revision), &mut ledger, request.now)?;
    Ok(ledger)
}

pub fn import_claimed_ledger(
    tx: &Tx<'_>,
    workspace_id: &str,
    ledger: &ReviewLedger,
) -> Result<ReviewLedger, OrbitError> {
    if ledger.lineage_key.is_empty() {
        return Err(OrbitError::InvalidInput(
            "review import requires a lineage key".into(),
        ));
    }
    let now = Utc::now();
    match tx.review().ledger(workspace_id, &ledger.lineage_key)? {
        None => {
            let mut incoming = ledger.clone();
            tx.review()
                .put_ledger(workspace_id, None, &mut incoming, now)?;
            Ok(incoming)
        }
        Some(local) if local.decisions.len() >= ledger.decisions.len() => Ok(local),
        Some(local) => {
            let revision = local.revision;
            let mut incoming = ledger.clone();
            tx.review()
                .put_ledger(workspace_id, Some(revision), &mut incoming, now)?;
            Ok(incoming)
        }
    }
}

pub fn reserve(
    tx: &Tx<'_>,
    workspace_id: &str,
    request: &ReviewReserveRequest<'_>,
) -> Result<(ReviewReservation, ReviewLedger), OrbitError> {
    let existing = tx.review().ledger(workspace_id, request.lineage_key)?;
    let previous_revision = existing.as_ref().map(|ledger| ledger.revision);
    let mut ledger = existing.unwrap_or_else(|| {
        ReviewLedger::new(
            request.lineage_key,
            request.task_ids.to_vec(),
            request.budget,
            request.now,
        )
    });

    // An interrupted attempt on the same candidate resumes while its
    // review has minutes left; on a different candidate it is partial
    // work, released as incomplete with the reviewer runtime already
    // spent.
    if let Some(open) = ledger.open_attempt().cloned() {
        if open.candidate == *request.candidate
            && open.task_meaning_digest == request.task_meaning_digest
        {
            if let Some(exhausted) = review_spent(&ledger, request) {
                return Ok((exhausted, ledger));
            }
            return Ok((ReviewReservation::Resumed { attempt: open }, ledger));
        }
        release_attempt(&mut ledger, &open.attempt_id, request.now, request.now);
        tx.review()
            .put_ledger(workspace_id, previous_revision, &mut ledger, request.now)?;
        return reserve_new(tx, workspace_id, ledger, request);
    }

    // Continue the latest unfinished review with the same attempt and
    // report identity; a timeout is not a final reviewer verdict.
    if let Some(attempt) = ledger
        .latest_attempt()
        .filter(|attempt| {
            attempt.released_at.is_some()
                && attempt.candidate == *request.candidate
                && attempt.task_meaning_digest == request.task_meaning_digest
        })
        .cloned()
    {
        if let Some(exhausted) = review_spent(&ledger, request) {
            return Ok((exhausted, ledger));
        }
        return Ok((ReviewReservation::Resumed { attempt }, ledger));
    }

    reserve_new_in(tx, workspace_id, previous_revision, ledger, request)
}

pub fn settle(
    tx: &Tx<'_>,
    workspace_id: &str,
    settlement: &ReviewSettlement<'_>,
) -> Result<ReviewLedger, OrbitError> {
    let mut ledger = tx
        .review()
        .ledger(workspace_id, settlement.lineage_key)?
        .ok_or_else(|| {
            OrbitError::Store(format!(
                "review lineage '{}' has no ledger to settle",
                settlement.lineage_key
            ))
        })?;
    let previous_revision = ledger.revision;
    if ledger
        .attempts
        .iter()
        .any(|a| a.attempt_id == settlement.attempt_id && a.index <= ledger.reset_through())
    {
        return Err(OrbitError::CapabilityDenied(
            "review attempt retired by an operator reset; admit a fresh attempt".into(),
        ));
    }
    let already_settled = ledger.attempts.iter().any(|attempt| {
        attempt.attempt_id == settlement.attempt_id
            && attempt.released_at.is_none()
            && matches!(attempt.state, ReviewAttemptState::Settled { .. })
    });
    if already_settled {
        return Ok(ledger);
    }
    if !settle_attempt(
        &mut ledger,
        settlement.attempt_id,
        settlement.verdict,
        settlement.now,
    ) {
        return Err(OrbitError::Store(format!(
            "review attempt '{}' is not part of lineage '{}'",
            settlement.attempt_id, settlement.lineage_key
        )));
    }
    tx.review().put_ledger(
        workspace_id,
        Some(previous_revision),
        &mut ledger,
        settlement.now,
    )?;
    Ok(ledger)
}

pub fn release(
    tx: &Tx<'_>,
    workspace_id: &str,
    release: &ReviewRelease<'_>,
) -> Result<ReviewLedger, OrbitError> {
    let mut ledger = tx
        .review()
        .ledger(workspace_id, release.lineage_key)?
        .ok_or_else(|| {
            OrbitError::Store(format!(
                "review lineage '{}' has no ledger to release",
                release.lineage_key
            ))
        })?;
    let previous_revision = ledger.revision;
    if ledger
        .attempts
        .iter()
        .any(|a| a.attempt_id == release.attempt_id && a.index <= ledger.reset_through())
    {
        return Err(OrbitError::CapabilityDenied(
            "review attempt retired by an operator reset; admit a fresh attempt".into(),
        ));
    }
    if !ledger
        .attempts
        .iter()
        .any(|attempt| attempt.attempt_id == release.attempt_id)
    {
        return Err(OrbitError::Store(format!(
            "review attempt '{}' is not part of lineage '{}'",
            release.attempt_id, release.lineage_key
        )));
    }
    if release_attempt(&mut ledger, release.attempt_id, release.bound, release.now) {
        tx.review().put_ledger(
            workspace_id,
            Some(previous_revision),
            &mut ledger,
            release.now,
        )?;
    }
    Ok(ledger)
}

pub fn release_run(
    tx: &Tx<'_>,
    workspace_id: &str,
    run_id: &str,
    finished_at: DateTime<Utc>,
) -> Result<Vec<ReviewLedger>, OrbitError> {
    let mut released = Vec::new();
    for mut ledger in tx.review().ledgers_held_by(workspace_id, run_id)? {
        let previous_revision = ledger.revision;
        let held = ledger
            .attempts
            .iter()
            .filter(|attempt| attempt.holder_run_id() == Some(run_id))
            .map(|attempt| attempt.attempt_id.clone())
            .collect::<Vec<_>>();
        let mut changed = false;
        for attempt_id in &held {
            changed |= release_attempt(&mut ledger, attempt_id, finished_at, finished_at);
        }
        if changed {
            tx.review().put_ledger(
                workspace_id,
                Some(previous_revision),
                &mut ledger,
                finished_at,
            )?;
            released.push(ledger);
        }
    }
    Ok(released)
}

pub fn record_invocation(
    tx: &Tx<'_>,
    workspace_id: &str,
    record: &ReviewInvocationRecord<'_>,
) -> Result<ReviewLedger, OrbitError> {
    let mut ledger = tx
        .review()
        .ledger(workspace_id, record.lineage_key)?
        .ok_or_else(|| {
            OrbitError::Store(format!(
                "review lineage '{}' has no ledger to record a reviewer invocation",
                record.lineage_key
            ))
        })?;
    let previous_revision = ledger.revision;
    if ledger
        .attempts
        .iter()
        .any(|a| a.attempt_id == record.attempt_id && a.index <= ledger.reset_through())
    {
        return Err(OrbitError::CapabilityDenied(
            "review attempt retired by an operator reset; admit a fresh attempt".into(),
        ));
    }
    if apply_invocation(
        &mut ledger,
        record.attempt_id,
        record.run_id,
        record.event,
        record.now,
    ) {
        tx.review().put_ledger(
            workspace_id,
            Some(previous_revision),
            &mut ledger,
            record.now,
        )?;
    }
    Ok(ledger)
}

pub fn record_certificate(
    tx: &Tx<'_>,
    workspace_id: &str,
    certificate: &ReviewCertificate,
) -> Result<(), OrbitError> {
    let review = tx.review();
    match review.certificate_matches(certificate)? {
        Some(true) => Ok(()),
        Some(false) => Err(OrbitError::InvalidInput(format!(
            "review certificate '{}' already recorded with different content",
            certificate.attempt_id
        ))),
        None => review.insert_certificate(workspace_id, certificate),
    }
}

pub fn open_reconciliation(
    tx: &Tx<'_>,
    workspace_id: &str,
    record: &ReviewReconciliation,
) -> Result<ReviewReconciliation, OrbitError> {
    let review = tx.review();
    if let Some(existing) = review.reconciliation_for_request(
        workspace_id,
        &record.binding.task_id,
        &record.request_key,
    )? {
        if existing.binding_digest != record.binding_digest {
            return Err(OrbitError::InvalidInput(format!(
                "review reconciliation request '{}' was already used for a different \
                 merged head or execution; choose a new request key",
                record.request_key
            )));
        }
        return Ok(existing);
    }
    let mut inserted = record.clone();
    inserted.revision = 1;
    inserted.updated_at = Utc::now();
    review.insert_reconciliation(workspace_id, &inserted)?;
    Ok(inserted)
}

pub fn update_reconciliation(
    tx: &Tx<'_>,
    workspace_id: &str,
    record: &ReviewReconciliation,
) -> Result<ReviewReconciliation, OrbitError> {
    let review = tx.review();
    let current = review
        .reconciliation(workspace_id, &record.reconciliation_id)?
        .ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "review reconciliation '{}' does not exist",
                record.reconciliation_id
            ))
        })?;
    if current.binding_digest != record.binding_digest
        || current.binding != record.binding
        || current.schema_version != record.schema_version
        || current.request_key != record.request_key
        || current.contract != record.contract
    {
        return Err(OrbitError::InvalidInput(
            "a review reconciliation's binding, schema, contract and request key are \
             immutable"
                .into(),
        ));
    }
    let mut updated = record.clone();
    updated.revision = record.revision.saturating_add(1);
    updated.updated_at = Utc::now();
    review.replace_reconciliation(workspace_id, record.revision, &updated)?;
    Ok(updated)
}
