//! Operator waivers for settled failed or exhausted batches.

use crate::AutomationError;
use chrono::{DateTime, Utc};
use orbit_store::contracts::AutomationStoreBackend;
use orbit_types::workflow::automation::*;

/// Waive only settled failed/exhausted work; retain its code as a coverage gap.
pub fn waive(
    store: &dyn AutomationStoreBackend,
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

    // Waiving is idempotent: a batch already recorded as waived needs no second write.
    if store
        .automation_waivers(consumer, 100)?
        .iter()
        .any(|w| w.batch_id == request.batch_id)
    {
        return Ok(());
    }

    let previous = store
        .automation_state(consumer)?
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
    next.generation = previous
        .generation
        .checked_add(1)
        .ok_or_else(|| AutomationError::Deferred("generation_exhausted".into()))?;
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

    if !store.automation_waive(&previous, &next, &waiver)? {
        return Err(AutomationError::Deferred("concurrent_evaluation".into()));
    }

    Ok(())
}
