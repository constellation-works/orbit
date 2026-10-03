//! Acknowledge a claimed member attempt with Core, or retire it as failed input.

use super::evaluate::{active_batch, diagnostic_with_batch, member_state};
use super::{MemberAdmission, MemberHost};
use crate::AutomationError;
use crate::checkpoint::commit;
use orbit_store::contracts::AutomationStoreBackend;
use orbit_types::workflow::automation::{members::*, *};

/// Acknowledge the active attempt with Core. `batch` carries the due reasons
/// of a claim made this pass; a resumed claim reports its members as admitted.
pub(super) fn admit(
    store: &dyn AutomationStoreBackend,
    host: &dyn MemberHost,
    state: AutomationState,
    dry_run: bool,
    batch: Option<Vec<BatchMember>>,
) -> Result<AutomationDiagnostic, AutomationError> {
    let consumer = state.consumer.clone();
    let active = state
        .members
        .as_ref()
        .and_then(|members| members.active.as_ref())
        .ok_or_else(|| AutomationError::Deferred("claim_missing".into()))?;

    let batch = batch.unwrap_or_else(|| active_batch(active));

    // Every member must still be admissible; `reconcile` shrinks the batch
    // to the ones that are on the next pass rather than failing siblings.
    for member in active.members() {
        match host.admission(member)? {
            MemberAdmission::Admit => {}
            MemberAdmission::Withhold(reason) | MemberAdmission::Retire(reason) => {
                return diagnostic_with_batch(store, &consumer, &reason, state, batch);
            }
        }
    }

    if dry_run {
        return diagnostic_with_batch(store, &consumer, "would_fire", state, batch);
    }

    let id = host.admit(active)?;

    let mut next = state.clone();
    if let Some(active) = &mut member_state(&mut next)?.active {
        active.action_id = Some(id);
    }

    let next = commit(store, &state, next, None)?;

    diagnostic_with_batch(store, &consumer, "fired", next, batch)
}

/// Record `attempt` as the exhausted failed input of every member it still
/// carries, so none of them refires at the same fingerprint.
pub(super) fn retire_attempt(members: &mut MemberState, mut attempt: MemberAttempt) {
    attempt.exhausted = true;
    for member in attempt.members().to_vec() {
        members.failed.insert(member.key, attempt.clone());
    }
}
