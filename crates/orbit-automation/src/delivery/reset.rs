//! The audited way to forget one delivery consumer's debt.
//!
//! Recovery exists to carry obligations across a change that cannot alter what
//! they mean. Reset is the opposite operation, and the only one that discards
//! them: it drops the consumer's whole position so the next evaluation seeds a
//! fresh baseline at the branch head, and it writes the inventory of what it
//! forgot into the same audit table recovery uses.
//!
//! It is deliberately the one operation with no compatibility refusals. A
//! consumer whose recorded identity cannot be proven at all — no trigger, no
//! frozen batch, a repository that moved — is exactly the consumer recovery
//! refuses, so reset must still work there. What it does refuse is destroying
//! state under a live action, unless the operator forces it.

use super::recovery;
use crate::AutomationError;
use chrono::{DateTime, Utc};
use orbit_store::contracts::AutomationStoreBackend;
use orbit_types::workflow::automation::recovery::*;
use orbit_types::workflow::automation::{
    AutomationState, BatchState, DeliveryTrigger, SourceRevision,
};

/// One reset request against one consumer, already resolved by the host.
pub struct Reset<'a> {
    pub consumer: &'a str,
    /// Epoch the definition resolves to now.
    pub epoch: &'a str,
    /// Resolved trigger behind that epoch.
    pub trigger: &'a DeliveryTrigger,
    /// A refusal the host already knows about, such as `owned_elsewhere`.
    pub host_refusal: Option<&'a str>,
    pub request: &'a ResetRequest,
    pub by: &'a str,
    pub now: DateTime<Utc>,
    /// Head of the configured branch the consumer re-baselines at.
    pub baseline: SourceRevision,
    /// Pinned `refs/orbit/automation/<consumer-digest>/*` refs the host holds
    /// for the forgotten batches. Recorded by an apply; empty on a preview.
    pub released_refs: Vec<String>,
}

/// Project what a reset would forget, without touching any state.
pub fn preview(
    store: &dyn AutomationStoreBackend,
    request: &Reset<'_>,
) -> Result<ResetPreview, AutomationError> {
    let state = recovery::load(store, request.consumer)?;
    project(store, request, &state, false)
}

/// Forget the consumer and record what was forgotten, in one transaction.
///
/// Any refusal aborts before the write, so a refused reset changes nothing.
pub fn apply(
    store: &dyn AutomationStoreBackend,
    request: &Reset<'_>,
) -> Result<ResetPreview, AutomationError> {
    let state = recovery::load(store, request.consumer)?;

    if !request.request.mutates() {
        return project(store, request, &state, false);
    }

    let refusals = refusals(request, &state);
    if !refusals.is_empty() {
        return Err(AutomationError::Refused(refusals.join(",")));
    }

    let record = record(store, request, &state)?;

    if !store.automation_reset(&state, &record)? {
        return Err(AutomationError::Deferred("concurrent_evaluation".into()));
    }

    // The state is gone, so the applied document reports the position the reset
    // destroyed rather than re-reading a consumer that no longer exists.
    project(store, request, &state, true)
}

/// The immutable audit of one reset: the identity it destroyed, the debt it
/// forgot, and the baseline the next evaluation starts from.
fn record(
    store: &dyn AutomationStoreBackend,
    request: &Reset<'_>,
    state: &AutomationState,
) -> Result<RecoveryRecord, AutomationError> {
    Ok(RecoveryRecord {
        consumer: state.consumer.clone(),
        previous_epoch: state.epoch.clone(),
        epoch: state.epoch.clone(),
        previous_trigger: state.trigger.clone(),
        trigger: state.trigger.clone(),
        adopted_settings: false,
        reissued: None,
        replayed_history: None,
        reset: Some(ResetRecord {
            previous_generation: state.generation,
            forgotten: recovery::debt(store, state)?,
            abandoned_action: recovery::stalled_action(store, state)?,
            baseline: request.baseline.clone(),
            released_refs: request.released_refs.clone(),
            cleared_stall: state.stall.clone(),
        }),
        friction_id: state
            .stall
            .as_ref()
            .and_then(|stall| stall.friction_id.clone()),
        reason: request.request.reason.trim().into(),
        by: request.by.trim().into(),
        at: request.now,
    })
}

/// Everything that forbids this reset, named deterministically.
///
/// Reset forgets the whole position, so nothing about the recorded identity —
/// trigger, coverage class, repository or branch — can refuse it.
fn refusals(request: &Reset<'_>, state: &AutomationState) -> Vec<String> {
    let mut refusals = vec![];

    if let Some(reason) = request.host_refusal {
        refusals.push(reason.to_string());
    }

    // Destroying the claim an executor is working against orphans its action.
    // The operator may still force it once they accept that outcome.
    if !request.request.force
        && state.active.as_ref().is_some_and(|active| {
            matches!(active.state, BatchState::Claimed | BatchState::Admitted)
        })
    {
        refusals.push(refusal::ACTION_EXECUTING.into());
    }

    if request.request.mutates() && request.by.trim().is_empty() {
        refusals.push(refusal::MISSING_AUTHORIZATION.into());
    }

    refusals
}

fn project(
    store: &dyn AutomationStoreBackend,
    request: &Reset<'_>,
    state: &AutomationState,
    applied: bool,
) -> Result<ResetPreview, AutomationError> {
    Ok(ResetPreview {
        consumer: state.consumer.clone(),
        reason: recovery::scheduling_reason(state, request.epoch, &request.trigger.branch),
        generation: state.generation,
        epoch: state.epoch.clone(),
        debt: recovery::debt(store, state)?,
        action: recovery::stalled_action(store, state)?,
        stall: state.stall.clone(),
        baseline: request.baseline.clone(),
        refusals: if applied {
            vec![]
        } else {
            refusals(request, state)
        },
        applied,
        history: store.automation_recoveries(request.consumer, recovery::HISTORY_LIMIT)?,
    })
}
