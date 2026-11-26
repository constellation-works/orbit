//! One generation-fenced checkpoint and receipt projection for all consumers.

mod members;
mod recovery;
mod transition;

use crate::{AutomationError, DeferredReason};
use orbit_common::OrbitError;
use orbit_store::Store;
use orbit_types::workflow::automation::*;
use orbit_types::workflow::automation::{BatchState, BatchWaiver, recovery::RecoveryRecord};

/// The generation a successor of `state` carries, or the deferral that stops
/// a write once the counter is exhausted.
pub(crate) fn next_generation(state: &AutomationState) -> Result<u64, AutomationError> {
    state
        .generation
        .checked_add(1)
        .ok_or_else(|| AutomationError::Deferred(DeferredReason::GenerationExhausted))
}

pub(crate) fn commit(
    store: &Store,
    old: &AutomationState,
    mut next: AutomationState,
    receipt: Option<&AcceptedCoverage>,
) -> Result<AutomationState, AutomationError> {
    next.generation = next_generation(old)?;

    if !store.write(|tx| crate::checkpoint::commit_state(tx, old, &next, receipt))? {
        return Err(AutomationError::Deferred(
            DeferredReason::ConcurrentEvaluation,
        ));
    }

    Ok(next)
}

pub(crate) fn diagnostic(
    store: &Store,
    consumer: &str,
    reason: &str,
    state: Option<AutomationState>,
) -> Result<AutomationDiagnostic, AutomationError> {
    Ok(AutomationDiagnostic {
        reason: reason.into(),
        state,
        // Ownership is resolved by the host that owns registry facts; this
        // shared evaluator only reports scheduling.
        ownership: None,
        batch: Vec::new(),
        refusals: Vec::new(),
        waivers: store
            .read()
            .and_then(|snapshot| snapshot.automation().waivers(consumer, 20))?,
        receipts: store
            .read()
            .and_then(|snapshot| snapshot.automation().receipts(consumer, 20))?
            .into_iter()
            .map(Into::into)
            .collect(),
    })
}

/// Insert the first checkpoint for a consumer.
pub fn initialize(tx: &orbit_store::Tx<'_>, state: &AutomationState) -> Result<bool, OrbitError> {
    if state
        .members
        .as_ref()
        .is_some_and(|members| members != &Default::default())
        || state.generation != 0
        || state.baseline != state.observed
        || state.baseline != state.covered
        || state.active.is_some()
        || state.stall.is_some()
        || !state.pending.is_empty()
        || !state.waived.is_empty()
        || !state.pending_commits.is_empty()
        || !state.unresolved.is_empty()
    {
        return Err(OrbitError::InvalidInput(
            "invalid automation baseline".into(),
        ));
    }

    tx.automation().insert_state(state)
}

/// Validate and advance a checkpoint, atomically recording accepted coverage.
pub fn commit_state(
    tx: &orbit_store::Tx<'_>,
    previous: &AutomationState,
    next: &AutomationState,
    receipt: Option<&AcceptedCoverage>,
) -> Result<bool, OrbitError> {
    transition::validate_transition(previous, next, receipt)?;
    let automation = tx.automation();
    if !automation.replace_state(previous, next)? {
        return Ok(false);
    }
    if let Some(receipt) = receipt {
        automation.insert_receipt(previous, receipt)?;
    }
    Ok(true)
}

/// Validate and record the retirement of a failed batch.
pub fn waive(
    tx: &orbit_store::Tx<'_>,
    previous: &AutomationState,
    next: &AutomationState,
    waiver: &BatchWaiver,
) -> Result<bool, OrbitError> {
    let invalid = || OrbitError::InvalidInput("invalid batch waiver transition".into());
    let active = previous.active.as_ref().ok_or_else(invalid)?;

    // Rebuild the only checkpoint this waiver may produce and require the caller
    // to have proposed exactly it.
    let mut expected = previous.clone();
    expected.generation = expected.generation.checked_add(1).ok_or_else(invalid)?;
    expected.active = None;
    expected.pending.retain(|delivery| {
        !active
            .batch
            .deliveries
            .iter()
            .any(|member| member.key == delivery.key)
    });
    expected.waived.extend(active.batch.deliveries.clone());

    if &expected != next
        || active.batch.id != waiver.batch_id
        || !matches!(active.state, BatchState::Failed | BatchState::Exhausted)
        || waiver.reason.trim().is_empty()
        || waiver.by.trim().is_empty()
    {
        return Err(invalid());
    }

    let automation = tx.automation();
    if !automation.replace_state(previous, next)? {
        return Ok(false);
    }
    automation.insert_waiver(&previous.consumer, active, waiver)?;
    Ok(true)
}

/// Advance a checkpoint under operator-authorized recovery and record its audit.
pub fn recover(
    tx: &orbit_store::Tx<'_>,
    previous: &AutomationState,
    next: &AutomationState,
    record: &RecoveryRecord,
) -> Result<bool, OrbitError> {
    recovery::validate(previous, next, record)?;
    let automation = tx.automation();
    if !automation.replace_state(previous, next)? {
        return Ok(false);
    }
    automation.insert_recovery(&previous.consumer, record)?;
    Ok(true)
}

/// Delete a checkpoint and record the debt it held.
pub fn reset(
    tx: &orbit_store::Tx<'_>,
    previous: &AutomationState,
    record: &RecoveryRecord,
) -> Result<bool, OrbitError> {
    recovery::validate_reset(previous, record)?;
    let automation = tx.automation();
    if !automation.delete_state(previous)? {
        return Ok(false);
    }
    automation.insert_recovery(&previous.consumer, record)?;
    Ok(true)
}

/// Record or clear a stall without moving the rest of the checkpoint.
pub fn stall(
    tx: &orbit_store::Tx<'_>,
    previous: &AutomationState,
    next: &AutomationState,
) -> Result<bool, OrbitError> {
    let invalid = || OrbitError::InvalidInput("invalid automation stall transition".into());

    if previous.generation.checked_add(1) != Some(next.generation) || previous.stall == next.stall {
        return Err(invalid());
    }

    let mut compared = next.clone();
    compared.generation = previous.generation;
    compared.stall = previous.stall.clone();
    if compared != *previous {
        return Err(invalid());
    }

    tx.automation().replace_state(previous, next)
}
