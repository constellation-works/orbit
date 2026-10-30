//! Bounded disjoint review reservations and contiguous receipt retirement.

use super::{
    DeliveryHost, Evaluation,
    adopt::{self, Adoption},
    evaluate::freeze_batch,
    observe, reconcile, stall,
};
use crate::{
    AutomationError, DeferredReason,
    checkpoint::{commit, diagnostic},
};
use orbit_store::contracts::AutomationStoreBackend;
use orbit_types::workflow::automation::*;

pub(super) fn evaluate_reserved(
    store: &dyn AutomationStoreBackend,
    host: &dyn DeliveryHost,
    request: Evaluation<'_>,
    mut state: AutomationState,
) -> Result<AutomationDiagnostic, AutomationError> {
    let Evaluation {
        consumer,
        epoch,
        trigger,
        enabled,
        dry_run,
        now,
    } = request;
    // Each batch resolves its own crash window and outcome. The generation CAS
    // precedes every external admission; replay uses the same durable action key.
    if !dry_run {
        let ids = state
            .delivery_attempts()
            .map(|a| a.batch.id.clone())
            .collect::<Vec<_>>();
        for id in ids {
            let attempt = state
                .delivery_attempts()
                .find(|a| a.batch.id == id)
                .cloned();
            if let Some(attempt) = attempt {
                if matches!(attempt.state, BatchState::Claimed | BatchState::Admitted)
                    && attempt.action_id.is_none()
                    && let Some(action_id) = host.action_id(&attempt)?
                {
                    let mut next = state.clone();
                    if let Some(a) = next.delivery_attempt_mut(&id) {
                        a.action_id = Some(action_id);
                    }
                    state = commit(store, &state, next, None)?;
                }
                if state
                    .delivery_attempts()
                    .any(|a| a.batch.id == id && a.action_id.is_some())
                {
                    state = reconcile::reconcile_batch(store, host, state, &id, now)?;
                }
            }
        }
        state = retire_reserved_prefix(store, state)?;
    }
    // Compatible edits affect future reservations only; frozen actions retain
    // their epoch, input, retry budget and executor provenance while running.
    if state.epoch != epoch || state.branch != trigger.branch {
        match adopt::adopt(store, host, &request, &state, true)? {
            Adoption::Adopted(next) => state = *next,
            Adoption::Refused(refusals) => {
                let mut result =
                    diagnostic(store, consumer, super::DEFINITION_CHANGED, Some(state))?;
                result.refusals = refusals;
                return Ok(result);
            }
        }
    }
    if !enabled {
        return diagnostic(store, consumer, "disabled", Some(state));
    }
    if let Some(suspended) = stall::suspended(store, host, &request, &mut state)? {
        return Ok(suspended);
    }
    if !dry_run {
        let claims = state
            .delivery_attempts()
            .filter(|a| a.state == BatchState::Claimed && a.action_id.is_none())
            .map(|a| a.batch.id.clone())
            .collect::<Vec<_>>();
        for id in claims {
            let attempt = state
                .delivery_attempts()
                .find(|a| a.batch.id == id)
                .cloned()
                .ok_or_else(|| AutomationError::Deferred(DeferredReason::MissingClaim))?;
            if attempt.attempt > 1 && now > attempt.deadline() {
                let mut next = state.clone();
                if let Some(a) = next.delivery_attempt_mut(&id) {
                    a.state = BatchState::Exhausted;
                    a.reason = Some("retry_deadline_expired".into());
                }
                state = commit(store, &state, next, None)?;
            } else if attempt.retry_after.is_none_or(|at| now >= at) {
                state = admit_reservation(store, host, &request, &state, &attempt)?;
                if let Some(held) = &state.stall {
                    return diagnostic(
                        store,
                        consumer,
                        &stall::stalled_reason(&held.reason),
                        Some(state.clone()),
                    );
                }
            }
        }
    }
    state = reconcile::retire_covered_prefix(store, state, dry_run)?;
    if state.pending.len() < 950 && state.pending_commits.len() <= 4800 {
        let page = host.observe(&trigger.branch, &state)?;
        let next = observe::apply(&state, page, trigger.coverage, now)?;
        state = if dry_run {
            next
        } else {
            commit(store, &state, next, None)?
        };
        state = reconcile::retire_covered_prefix(store, state, dry_run)?;
    }
    let pending = state.unreserved_deliveries().collect::<Vec<_>>();
    let due_count = pending.len() >= trigger.threshold;
    let due_age = pending.first().is_some_and(|d| {
        now.signed_duration_since(d.landed_at).num_minutes() >= i64::from(trigger.max_wait_minutes)
    });
    if state.delivery_attempts().count() >= MAX_DELIVERY_RESERVATIONS {
        return diagnostic(store, consumer, "reservation_capacity", Some(state));
    }
    if !due_count && !due_age {
        let reason = if state
            .delivery_attempts()
            .any(|a| matches!(a.state, BatchState::Failed | BatchState::Exhausted))
        {
            "needs_attention"
        } else if state.active.is_some() {
            "batch_pending"
        } else if !state.unresolved.is_empty() {
            "evidence_unavailable"
        } else {
            "not_due"
        };
        return diagnostic(store, consumer, reason, Some(state));
    }
    if let Some(reason) = host.admission_deferral()? {
        return diagnostic(store, consumer, &reason, Some(state));
    }
    let reason = if due_count {
        "threshold_reached"
    } else {
        "max_wait_reached"
    };
    if !dry_run {
        let Some(attempt) = freeze_batch(&state, &request)? else {
            return diagnostic(store, consumer, "batch_too_large", Some(state));
        };
        let mut next = state.clone();
        if next.active.is_none() {
            next.active = Some(attempt.clone());
        } else {
            next.reservations.push(attempt.clone());
        }
        state = commit(store, &state, next, None)?;
        state = admit_reservation(store, host, &request, &state, &attempt)?;
        if let Some(held) = &state.stall {
            return diagnostic(
                store,
                consumer,
                &stall::stalled_reason(&held.reason),
                Some(state.clone()),
            );
        }
    }
    diagnostic(store, consumer, reason, Some(state))
}

fn admit_reservation(
    store: &dyn AutomationStoreBackend,
    host: &dyn DeliveryHost,
    request: &Evaluation<'_>,
    state: &AutomationState,
    attempt: &BatchAttempt,
) -> Result<AutomationState, AutomationError> {
    if let Some(hold) = host.admission_hold(attempt)? {
        // This is an actual resource/configuration hold, rather than another
        // review's completion. Keep the claim and its immutable input.
        return stall::held(store, host, request, state, hold)?
            .state
            .ok_or_else(|| AutomationError::Deferred(DeferredReason::MissingClaim));
    }
    let id = host.admit(attempt)?;
    let mut next = state.clone();
    if let Some(a) = next.delivery_attempt_mut(&attempt.batch.id) {
        a.action_id = Some(id);
        a.state = BatchState::Admitted;
    }
    commit(store, state, next, None)
}

/// Receipt acceptance and cursor advancement are separate atomic transitions.
/// Covered reservations behind a hole survive restarts without another review.
fn retire_reserved_prefix(
    store: &dyn AutomationStoreBackend,
    mut state: AutomationState,
) -> Result<AutomationState, AutomationError> {
    while let Some(active) = state
        .active
        .as_ref()
        .filter(|a| a.state == BatchState::Covered)
    {
        let mut next = state.clone();
        let commits = &active.batch.commits;
        next.covered = active.batch.through_inclusive.clone();
        next.pending_commits = next.pending_commits[commits.len()..].to_vec();
        next.pending
            .retain(|d| !d.commits.iter().all(|sha| commits.contains(sha)));
        next.waived
            .retain(|d| !d.commits.iter().all(|sha| commits.contains(sha)));
        next.excluded
            .retain(|d| !d.delivery.commits.iter().all(|sha| commits.contains(sha)));
        next.unresolved.retain(|sha, _| !commits.contains(sha));
        next.associations.retain(|sha, _| !commits.contains(sha));
        next.lookup_retries.retain(|sha, _| !commits.contains(sha));
        next.active = if next.reservations.is_empty() {
            None
        } else {
            Some(next.reservations.remove(0))
        };
        state = commit(store, &state, next, None)?;
    }
    Ok(state)
}
