//! Selection and inspection of independent retained reservations.

use super::recovery::stalled_action;
use crate::{AutomationError, DeferredReason};
use orbit_store::contracts::AutomationStoreBackend;
use orbit_types::workflow::automation::{recovery::*, *};

/// Snapshot of only the selected batch for host liveness probes. Never persist
/// this projection: durable recovery always compares the entire consumer.
pub fn recovery_selection(state: &AutomationState, request: &RecoveryRequest) -> AutomationState {
    let mut selected = state.clone();
    selected.active = selected_attempt(state, request).cloned();
    selected.reservations.clear();
    selected
}

pub(super) fn selected_attempt<'a>(
    state: &'a AutomationState,
    request: &RecoveryRequest,
) -> Option<&'a BatchAttempt> {
    match request.batch_id.as_deref() {
        Some(id) => state.delivery_attempts().find(|a| a.batch.id == id),
        None => state.active.as_ref(),
    }
}

/// Every retained reservation and its receipt status, without treating one
/// action's liveness as proof about another action.
pub(super) fn retained_actions(
    store: &dyn AutomationStoreBackend,
    state: &AutomationState,
) -> Result<Vec<StalledAction>, AutomationError> {
    if !state.independent_review {
        return Ok(vec![]);
    }
    state
        .delivery_attempts()
        .map(|attempt| {
            let mut selected = state.clone();
            selected.active = Some(attempt.clone());
            selected.reservations.clear();
            stalled_action(store, &selected, false)?
                .ok_or_else(|| AutomationError::Deferred(DeferredReason::MissingClaim))
        })
        .collect()
}
