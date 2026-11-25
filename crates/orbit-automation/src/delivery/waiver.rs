//! Operator waivers for settled failed or exhausted batches.

use crate::{AutomationError, DeferredReason, checkpoint};
use chrono::{DateTime, Utc};
use orbit_store::Store;
use orbit_types::workflow::automation::*;

/// Waive only settled failed/exhausted work; retain its code as a coverage gap.
pub fn waive(
    store: &Store,
    consumer: &str,
    request: &WaiveBatchRequest,
    by: &str,
    now: DateTime<Utc>,
) -> Result<(), AutomationError> {
    if request.reason.trim().is_empty() || by.trim().is_empty() {
        return Err(AutomationError::Evidence(
            "waiver requires a reason and actor".into(),
        ));
    }

    // Waiving is idempotent: a batch already recorded as waived needs no second
    // write. Look it up by its durable identity, not among a recent page.
    if store
        .read()
        .and_then(|snapshot| snapshot.automation().waiver(consumer, &request.batch_id))?
        .is_some()
    {
        return Ok(());
    }

    let previous = store
        .read()
        .and_then(|snapshot| snapshot.automation().state(consumer))?
        .ok_or_else(|| AutomationError::Evidence("consumer not found".into()))?;

    let active = previous
        .active
        .as_ref()
        .ok_or_else(|| AutomationError::Evidence("no active batch".into()))?;

    if active.batch.id != request.batch_id
        || !matches!(active.state, BatchState::Failed | BatchState::Exhausted)
    {
        return Err(AutomationError::Evidence(
            "only the current settled failed or exhausted batch can be waived".into(),
        ));
    }

    let mut next = previous.clone();
    next.generation = checkpoint::next_generation(&previous)?;
    next.active = None;
    next.pending.retain(|delivery| {
        !active
            .batch
            .deliveries
            .iter()
            .any(|member| member.key == delivery.key)
    });
    next.waived.extend(active.batch.deliveries.clone());

    let waiver = BatchWaiver {
        batch_id: request.batch_id.clone(),
        reason: request.reason.clone(),
        by: by.into(),
        at: now,
    };

    if !store.write(|tx| crate::checkpoint::waive(tx, &previous, &next, &waiver))? {
        return Err(AutomationError::Deferred(
            DeferredReason::ConcurrentEvaluation,
        ));
    }

    Ok(())
}
