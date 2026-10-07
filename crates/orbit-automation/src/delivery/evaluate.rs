//! One evaluation pass: baseline, reconcile, observe, then admit what is due.

use super::adopt::{Adoption, adopt};
use super::reconcile::{reconcile, retire_covered_prefix};
use super::{DEFINITION_CHANGED, DeliveryHost, Evaluation, input_digest, observe, stall};
use crate::AutomationError;
use crate::checkpoint::{commit, diagnostic};
use orbit_store::contracts::AutomationStoreBackend;
use orbit_types::workflow::automation::*;

/// Evaluate explicit configuration without another scheduler or ticking loop.
///
/// A deferred reason that survives the pass is classified rather than simply
/// returned: `stall` decides whether the next tick may retry it silently, or
/// whether the consumer has to stop and say so. Preview never writes, so it
/// reports the deferral exactly as the evaluator saw it.
pub fn evaluate(
    store: &dyn AutomationStoreBackend,
    host: &dyn DeliveryHost,
    request: Evaluation<'_>,
) -> Result<AutomationDiagnostic, AutomationError> {
    match evaluate_pass(store, host, request) {
        Err(AutomationError::Deferred(reason)) if !request.dry_run => {
            stall::deferred(store, host, &request, reason)
        }
        outcome => outcome,
    }
}

/// One ordinary evaluation pass: reconcile, observe, then admit what is due.
fn evaluate_pass(
    store: &dyn AutomationStoreBackend,
    host: &dyn DeliveryHost,
    request: Evaluation<'_>,
) -> Result<AutomationDiagnostic, AutomationError> {
    let Evaluation {
        consumer,
        epoch,
        trigger,
        enabled,
        dry_run,
        now,
    } = request;

    trigger.validate().map_err(orbit_common::OrbitError::from)?;

    // The first evaluation pins a baseline at the branch head; nothing before it is debt.
    let mut state = match store.automation_state(consumer)? {
        Some(state) => state,
        None => {
            // Disabled definitions never pin a baseline. Preview must match the
            // real pass here: probing git would invent `would_baseline` or an
            // `evidence_unavailable` error the scheduled tick never sees.
            if !enabled {
                return diagnostic(store, consumer, "disabled", None);
            }

            let (repository, head) = host.head(&trigger.branch)?;
            let state = AutomationState {
                members: None,
                consumer: consumer.into(),
                epoch: epoch.into(),
                trigger: Some(trigger.clone()),
                repository,
                branch: trigger.branch.clone(),
                generation: 0,
                baseline: head.clone(),
                observed: head.clone(),
                covered: head,
                pending_commits: vec![],
                pending: vec![],
                waived: vec![],
                excluded: vec![],
                unresolved: Default::default(),
                associations: Default::default(),
                active: None,
                stall: None,
            };

            if !dry_run {
                store.automation_initialize(&state)?;
            }

            return diagnostic(
                store,
                consumer,
                if dry_run {
                    "would_baseline"
                } else {
                    "baselined"
                },
                Some(state),
            );
        }
    };

    // Resolve minted claims and reconcile even when disabled or edited. A
    // crash can leave a task behind before its action id was checkpointed.
    let mut retry_scheduled = false;
    if !dry_run && let Some(active) = &state.active {
        if matches!(active.state, BatchState::Claimed | BatchState::Admitted)
            && active.action_id.is_none()
            && let Some(id) = host.action_id(active)?
        {
            let mut next = state.clone();
            if let Some(active) = &mut next.active {
                active.action_id = Some(id);
            }
            state = commit(store, &state, next, None)?;
        }
        if state
            .active
            .as_ref()
            .is_some_and(|active| active.action_id.is_some())
        {
            let previous_attempt = state.active.as_ref().map(|active| active.attempt);
            state = reconcile(store, host, state, now)?;
            // A retry just scheduled by settlement has no executing action.
            // It can carry the compatible settings forward in this pass,
            // before admission, while retaining its frozen batch and backoff.
            retry_scheduled = state.active.as_ref().is_some_and(|active| {
                active.state == BatchState::Claimed
                    && active.action_id.is_none()
                    && Some(active.attempt) != previous_attempt
            });
        }
    }

    // A settings-only edit is adopted in place and the pass carries on; any
    // other edit holds the consumer and names why.
    if state.epoch != epoch || state.branch != trigger.branch {
        match adopt(store, host, &request, &state, retry_scheduled)? {
            Adoption::Adopted(adopted) => state = *adopted,
            Adoption::Refused(refusals) => {
                let mut changed = diagnostic(store, consumer, DEFINITION_CHANGED, Some(state))?;
                changed.refusals = refusals;
                return Ok(changed);
            }
        }
    }

    if !enabled {
        return diagnostic(store, consumer, "disabled", Some(state));
    }

    // A consumer an operator has to repair observes nothing: retrying the same
    // unprovable source fact every minute is what hid this debt before. Work
    // already admitted was reconciled above, so evidence can still arrive.
    if let Some(suspended) = stall::suspended(store, host, &request, &mut state)? {
        return Ok(suspended);
    }

    // A claim that never reached admission resumes here, subject to its retry budget.
    if let Some(active) = &state.active
        && active.action_id.is_none()
        && active.state == BatchState::Claimed
        && !dry_run
    {
        if active.attempt > 1 && now > active.deadline() {
            let mut next = state.clone();
            if let Some(attempt) = &mut next.active {
                attempt.state = BatchState::Exhausted;
                attempt.reason = Some("retry_deadline_expired".into());
            }

            state = commit(store, &state, next, None)?;

            return diagnostic(store, consumer, "retry_deadline_expired", Some(state));
        }

        if active.retry_after.is_some_and(|at| now < at) {
            return diagnostic(store, consumer, "retry_backoff", Some(state));
        }

        state = admit_active(store, host, &state)?;
    }

    // Proven-covered prefixes are not debt. Retire them before backpressure so
    // an already-stalled excluded-only consumer can observe again.
    state = retire_covered_prefix(store, state, dry_run)?;

    // Backpressure pauses observation, never admission of already retained debt.
    if state.pending.len() < 950 && state.pending_commits.len() <= 4800 {
        let page = host.observe(&trigger.branch, &state)?;
        let next = observe::apply(&state, page, trigger.coverage, now)?;
        state = if dry_run {
            next
        } else {
            commit(store, &state, next, None)?
        };
        state = retire_covered_prefix(store, state, dry_run)?;
    }

    if let Some(active) = &state.active {
        let reason = match active.state {
            BatchState::Failed => "batch_failed",
            BatchState::Exhausted => "needs_attention",
            _ => "batch_pending",
        };
        return diagnostic(store, consumer, reason, Some(state));
    }

    let due_count = state.pending.len() >= trigger.threshold;
    let due_age = state.pending.first().is_some_and(|d| {
        now.signed_duration_since(d.landed_at).num_minutes() >= i64::from(trigger.max_wait_minutes)
    });

    if !due_count && !due_age {
        return diagnostic(
            store,
            consumer,
            if state.unresolved.is_empty() {
                "not_due"
            } else {
                "evidence_unavailable"
            },
            Some(state),
        );
    }

    if let Some(reason) = host.admission_deferral()? {
        return diagnostic(store, consumer, &reason, Some(state));
    }

    if dry_run {
        return diagnostic(
            store,
            consumer,
            if due_count {
                "threshold_reached"
            } else {
                "max_wait_reached"
            },
            Some(state),
        );
    }

    // Freeze an oldest prefix. Unattributed neighbors remain explicit obligations.
    let deliveries = state
        .pending
        .iter()
        .take(trigger.max_items)
        .cloned()
        .collect::<Vec<_>>();

    let through = if state.pending.len() > deliveries.len() {
        deliveries
            .last()
            .ok_or_else(|| AutomationError::Deferred("empty_batch".into()))?
            .after
            .clone()
    } else {
        state.observed.clone()
    };

    let end = state
        .pending_commits
        .iter()
        .position(|sha| sha == &through.commit)
        .ok_or_else(|| AutomationError::Deferred("delivery_boundary_missing".into()))?
        + 1;

    let commits = state.pending_commits[..end].to_vec();

    if deliveries
        .iter()
        .any(|delivery| !delivery.commits.iter().all(|sha| commits.contains(sha)))
    {
        return Err(AutomationError::Deferred(
            "delivery_crosses_boundary".into(),
        ));
    }

    // Excluded landings inside the range travel with the batch as readable
    // context; they are not obligations and the evidence never lists them.
    let exclusions = state
        .excluded
        .iter()
        .filter(|excluded| {
            excluded
                .delivery
                .commits
                .iter()
                .all(|sha| commits.contains(sha))
        })
        .cloned()
        .collect::<Vec<_>>();

    let mut batch = CoverageBatch {
        schema_version: 1,
        id: String::new(),
        consumer: consumer.into(),
        epoch: epoch.into(),
        repository: state.repository.clone(),
        branch: state.branch.clone(),
        coverage: trigger.coverage,
        from_exclusive: state.covered.clone(),
        through_inclusive: through,
        commits,
        deliveries,
        exclusions,
        created_at: now,
        max_attempts: trigger.retries + 1,
        retry_until: now + chrono::Duration::hours(24),
    };

    // The identity digest covers the batch before it carries an id; the frozen input
    // digest then covers the identified batch that evidence must be checked against.
    batch.id = input_digest(&batch)?;
    let frozen_input_digest = input_digest(&batch)?;

    if serde_json::to_vec(&batch)
        .map_err(|e| AutomationError::Evidence(e.to_string()))?
        .len()
        > 1_048_576
    {
        return diagnostic(store, consumer, "batch_too_large", Some(state));
    }

    let attempt = BatchAttempt {
        action_key: format!("automation:{}:1", batch.id),
        batch,
        input_digest: frozen_input_digest,
        attempt: 1,
        action_id: None,
        state: BatchState::Claimed,
        reason: None,
        retry_after: None,
        reissue: None,
    };

    let mut next = state.clone();
    next.active = Some(attempt);
    state = commit(store, &state, next, None)?;

    state = admit_active(store, host, &state)?;

    diagnostic(
        store,
        consumer,
        if due_count {
            "threshold_reached"
        } else {
            "max_wait_reached"
        },
        Some(state),
    )
}

/// Hand the claimed attempt to Core and record the durable action id it resolved.
fn admit_active(
    store: &dyn AutomationStoreBackend,
    host: &dyn DeliveryHost,
    state: &AutomationState,
) -> Result<AutomationState, AutomationError> {
    let active = state
        .active
        .as_ref()
        .ok_or_else(|| AutomationError::Deferred("missing_claim".into()))?;
    let id = host.admit(active)?;

    let mut next = state.clone();
    if let Some(attempt) = &mut next.active {
        attempt.action_id = Some(id);
        attempt.state = BatchState::Admitted;
    }

    commit(store, state, next, None)
}
