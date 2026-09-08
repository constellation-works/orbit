//! One generation-fenced checkpoint and receipt projection for all consumers.

use crate::AutomationError;
use orbit_store::contracts::AutomationStoreBackend;
use orbit_types::workflow::automation::*;

pub(crate) fn commit(
    store: &dyn AutomationStoreBackend,
    old: &AutomationState,
    mut next: AutomationState,
    receipt: Option<&AcceptedCoverage>,
) -> Result<AutomationState, AutomationError> {
    next.generation = old
        .generation
        .checked_add(1)
        .ok_or_else(|| AutomationError::Deferred("generation_exhausted".into()))?;

    if !store.automation_commit(old, &next, receipt)? {
        return Err(AutomationError::Deferred("concurrent_evaluation".into()));
    }

    Ok(next)
}

pub(crate) fn diagnostic(
    store: &dyn AutomationStoreBackend,
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
        waivers: store.automation_waivers(consumer, 20)?,
        receipts: store
            .automation_receipts(consumer, 20)?
            .into_iter()
            .map(Into::into)
            .collect(),
    })
}
