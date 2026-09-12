//! Explicit, audited recovery for a delivery consumer stalled by a settings
//! change [ORB-12295].
//!
//! Editing a definition moves its epoch, so the consumer stops admitting work
//! while every obligation it already holds stays retained. The supported way
//! forward is this operation, not a hand-edited state file: it adopts the new
//! configuration identity when the change cannot alter what the retained debt
//! means, and it reissues an action that settled without accepted evidence
//! over the exact obligations already frozen for it.
//!
//! Nothing here covers, waives or discards debt. Coverage still requires valid
//! evidence from the assigned executor of an admitted action.

use crate::AutomationError;
use chrono::{DateTime, Utc};
use orbit_store::contracts::AutomationStoreBackend;
use orbit_types::workflow::automation::recovery::*;
use orbit_types::workflow::automation::*;

/// How many audited recoveries a preview reports.
const HISTORY_LIMIT: usize = 10;

/// One additional attempt is authorized for the same window the frozen batch
/// budget grants, so a reissue never quietly widens the retry deadline policy.
const REISSUE_WINDOW_HOURS: i64 = 24;

/// One recovery request against one consumer, already resolved by the host.
pub struct Recovery<'a> {
    pub consumer: &'a str,
    /// Epoch the definition resolves to now.
    pub epoch: &'a str,
    /// Resolved trigger behind that epoch.
    pub trigger: &'a DeliveryTrigger,
    /// Repository identity the source adapter reports for the configured
    /// branch now.
    pub repository: &'a str,
    /// A refusal the host already knows about, such as `owned_elsewhere`.
    /// Recorded rather than re-derived; Automation owns no registry facts.
    pub host_refusal: Option<&'a str>,
    pub request: &'a RecoveryRequest,
    pub by: &'a str,
    pub now: DateTime<Utc>,
}

/// Project the consumer's recovery position without touching any state.
///
/// `refusals` always reports what would block a recovery of this consumer at
/// all; the refusals specific to an operation are added only when the request
/// asks for that operation.
pub fn preview(
    store: &dyn AutomationStoreBackend,
    request: &Recovery<'_>,
) -> Result<RecoveryPreview, AutomationError> {
    let state = load(store, request.consumer)?;
    project(store, request, request.request, &state, vec![])
}

/// Apply the requested recovery under a generation fence, retaining every
/// covered, pending, unresolved, waived, excluded and accepted fact.
///
/// Any refusal aborts before the write, so a refused recovery changes nothing.
pub fn apply(
    store: &dyn AutomationStoreBackend,
    request: &Recovery<'_>,
) -> Result<RecoveryPreview, AutomationError> {
    let state = load(store, request.consumer)?;

    if !request.request.mutates() {
        return project(store, request, request.request, &state, vec![]);
    }

    let refusals = refusals(store, request, request.request, &state)?;
    if !refusals.is_empty() {
        return Err(AutomationError::Refused(refusals.join(",")));
    }

    let (next, record) = plan(request, &state)?;

    if !store.automation_recover(&state, &next, &record)? {
        return Err(AutomationError::Deferred("concurrent_evaluation".into()));
    }

    let mut applied = vec![];
    if record.adopted_settings {
        applied.push(RecoveryPreview::ADOPTED_SETTINGS.to_string());
    }
    if record.reissued.is_some() {
        applied.push(RecoveryPreview::REISSUED_ACTION.to_string());
    }

    // The applied document reports the position the recovery left behind, so
    // its refusals describe the consumer rather than the request just settled.
    project(store, request, &RecoveryRequest::default(), &next, applied)
}

fn load(
    store: &dyn AutomationStoreBackend,
    consumer: &str,
) -> Result<AutomationState, AutomationError> {
    let state = store
        .automation_state(consumer)?
        .ok_or_else(|| AutomationError::Refused(refusal::UNKNOWN_CONSUMER.into()))?;

    if state.members.is_some() {
        return Err(AutomationError::Refused(refusal::MEMBER_CONSUMER.into()));
    }

    Ok(state)
}

/// Build the durable next state and its audit record. Only the configuration
/// identity and the settled attempt may move; every cursor and obligation is
/// carried over untouched.
fn plan(
    request: &Recovery<'_>,
    state: &AutomationState,
) -> Result<(AutomationState, RecoveryRecord), AutomationError> {
    let mut next = state.clone();
    next.generation = state
        .generation
        .checked_add(1)
        .ok_or_else(|| AutomationError::Deferred("generation_exhausted".into()))?;

    if request.request.adopt_settings {
        next.epoch = request.epoch.into();
        next.trigger = Some(request.trigger.clone());
    }

    let reissued = if request.request.reissue_action {
        Some(reissue(request, &mut next)?)
    } else {
        None
    };

    let record = RecoveryRecord {
        consumer: state.consumer.clone(),
        previous_epoch: state.epoch.clone(),
        epoch: next.epoch.clone(),
        previous_trigger: state.trigger.clone(),
        trigger: next.trigger.clone(),
        adopted_settings: request.request.adopt_settings,
        reissued,
        reason: request.request.reason.trim().into(),
        by: request.by.trim().into(),
        at: request.now,
    };

    Ok((next, record))
}

/// Replace the settled attempt with one authorized attempt over the identical
/// frozen batch. The previous action is left exactly as it settled.
fn reissue(
    request: &Recovery<'_>,
    next: &mut AutomationState,
) -> Result<ReissuedAction, AutomationError> {
    let attempt = next
        .active
        .as_mut()
        .ok_or_else(|| AutomationError::Refused(refusal::NO_SETTLED_ACTION.into()))?;

    let from_action_id = attempt.action_id.take();
    let from_attempt = attempt.attempt;
    let from_state = attempt.state;
    let from_reason = attempt.reason.take();

    let authorization = ActionReissue {
        from_action_id: from_action_id.clone(),
        reason: request.request.reason.trim().into(),
        by: request.by.trim().into(),
        at: request.now,
        retry_until: request.now + chrono::Duration::hours(REISSUE_WINDOW_HOURS),
    };

    attempt.attempt = from_attempt
        .checked_add(1)
        .ok_or_else(|| AutomationError::Deferred("attempt_exhausted".into()))?;
    attempt.action_key = format!("automation:{}:{}", attempt.batch.id, attempt.attempt);
    attempt.state = BatchState::Claimed;
    attempt.retry_after = None;
    attempt.reissue = Some(authorization.clone());

    Ok(ReissuedAction {
        batch_id: attempt.batch.id.clone(),
        from_action_id,
        from_attempt,
        from_state,
        from_reason,
        attempt: attempt.attempt,
        authorization,
    })
}

/// Everything that forbids `requested`, named deterministically. Refusals that
/// block any recovery of this consumer are always reported; the ones specific
/// to an operation only when it was asked for.
fn refusals(
    store: &dyn AutomationStoreBackend,
    request: &Recovery<'_>,
    requested: &RecoveryRequest,
    state: &AutomationState,
) -> Result<Vec<String>, AutomationError> {
    let mut refusals = vec![];
    let mut refuse = |reason: &str| refusals.push(reason.to_string());

    if let Some(reason) = request.host_refusal {
        refuse(reason);
    }

    // Adoption may only carry debt across a change that cannot alter what the
    // debt means: the same repository, branch, owner and examination contract.
    if state.branch != request.trigger.branch {
        refuse(refusal::BRANCH_CHANGED);
    }

    if state.repository != request.repository {
        refuse(refusal::REPOSITORY_CHANGED);
    }

    if state
        .trigger
        .as_ref()
        .is_some_and(|recorded| recorded.owner_machine != request.trigger.owner_machine)
    {
        refuse(refusal::OWNER_CHANGED);
    }

    match recorded_coverage(state) {
        Some(coverage) if coverage != request.trigger.coverage => refuse(refusal::COVERAGE_CHANGED),
        Some(_) => {}
        None => refuse(refusal::COVERAGE_UNVERIFIABLE),
    }

    // Live execution is never interrupted; its action has to settle first.
    if state
        .active
        .as_ref()
        .is_some_and(|active| matches!(active.state, BatchState::Claimed | BatchState::Admitted))
    {
        refuse(refusal::ACTIVE_EXECUTION);
    }

    if requested.mutates()
        && (request.request.reason.trim().is_empty() || request.by.trim().is_empty())
    {
        refuse(refusal::MISSING_AUTHORIZATION);
    }

    if requested.adopt_settings
        && state.epoch == request.epoch
        && state.trigger.as_ref() == Some(request.trigger)
    {
        refuse(refusal::SETTINGS_UNCHANGED);
    }

    if requested.reissue_action {
        match state.active.as_ref() {
            Some(active) if matches!(active.state, BatchState::Failed | BatchState::Exhausted) => {
                if store
                    .automation_receipt(&state.consumer, &active.batch.id)?
                    .is_some()
                {
                    refuse(refusal::ACTION_EVIDENCED);
                }
            }
            _ => refuse(refusal::NO_SETTLED_ACTION),
        }

        // A reissued claim admits nothing while the recorded identity is stale,
        // so the same request has to adopt the configuration it will run under.
        if !requested.adopt_settings && state.epoch != request.epoch {
            refuse(refusal::DEFINITION_CHANGED);
        }
    }

    Ok(refusals)
}

/// The examination contract the retained debt was accumulated under. The
/// recorded trigger answers; otherwise the frozen batch does, and a consumer
/// with neither cannot prove it.
fn recorded_coverage(state: &AutomationState) -> Option<CoverageClass> {
    state
        .trigger
        .as_ref()
        .map(|trigger| trigger.coverage)
        .or_else(|| state.active.as_ref().map(|active| active.batch.coverage))
}

fn project(
    store: &dyn AutomationStoreBackend,
    request: &Recovery<'_>,
    requested: &RecoveryRequest,
    state: &AutomationState,
    applied: Vec<String>,
) -> Result<RecoveryPreview, AutomationError> {
    let receipts = store.automation_receipts(request.consumer, 100)?;
    let reissuable = |active: &BatchAttempt| {
        matches!(active.state, BatchState::Failed | BatchState::Exhausted)
            && !receipts
                .iter()
                .any(|receipt| receipt.batch_id == active.batch.id)
    };

    Ok(RecoveryPreview {
        consumer: state.consumer.clone(),
        reason: stall_reason(state, request).into(),
        identity: RecoveryIdentity {
            recorded_epoch: state.epoch.clone(),
            configured_epoch: request.epoch.into(),
            changes: changes(state, request),
            recorded_trigger: state.trigger.clone(),
            configured_trigger: request.trigger.clone(),
        },
        debt: CoverageDebt {
            baseline: state.baseline.clone(),
            covered: state.covered.clone(),
            observed: state.observed.clone(),
            pending_deliveries: state.pending.len(),
            pending_commits: state.pending_commits.len(),
            unresolved: state.unresolved.len(),
            waived: state.waived.len(),
            excluded: state.excluded.len(),
            receipts: receipts.len(),
        },
        action: state.active.as_ref().map(|active| StalledAction {
            batch_id: active.batch.id.clone(),
            attempt: active.attempt,
            state: active.state,
            action_id: active.action_id.clone(),
            reason: active.reason.clone(),
            obligations: active
                .batch
                .deliveries
                .iter()
                .map(|delivery| delivery.key.clone())
                .collect(),
            commits: active.batch.commits.len(),
            reissuable: reissuable(active),
        }),
        refusals: refusals(store, request, requested, state)?,
        applied,
        history: store.automation_recoveries(request.consumer, HISTORY_LIMIT)?,
    })
}

/// Why this consumer is or is not stalled, in the evaluator's own precedence:
/// an edited definition first, then a settled action needing attention.
fn stall_reason(state: &AutomationState, request: &Recovery<'_>) -> &'static str {
    if state.epoch != request.epoch || state.branch != request.trigger.branch {
        return super::DEFINITION_CHANGED;
    }

    match state.active.as_ref().map(|active| active.state) {
        Some(BatchState::Failed | BatchState::Exhausted) => "needs_attention",
        Some(_) => "batch_pending",
        None => "not_stalled",
    }
}

/// Name each configured setting that differs from the recorded one. A consumer
/// that never recorded its trigger can only report that the identity differs.
fn changes(state: &AutomationState, request: &Recovery<'_>) -> Vec<String> {
    let Some(recorded) = state.trigger.as_ref() else {
        return if state.epoch == request.epoch {
            vec![]
        } else {
            vec!["configuration_identity".into()]
        };
    };

    let configured = request.trigger;
    let mut changes = vec![];

    for (name, differs) in [
        (
            "owner_machine",
            recorded.owner_machine != configured.owner_machine,
        ),
        ("branch", recorded.branch != configured.branch),
        ("coverage", recorded.coverage != configured.coverage),
        ("threshold", recorded.threshold != configured.threshold),
        (
            "max_wait_minutes",
            recorded.max_wait_minutes != configured.max_wait_minutes,
        ),
        ("max_items", recorded.max_items != configured.max_items),
        ("retries", recorded.retries != configured.retries),
    ] {
        if differs {
            changes.push(name.to_string());
        }
    }

    // Whatever else the epoch covers — the task template, dedupe policy or a
    // routine's target and policy — is visible only as a different epoch.
    if changes.is_empty() && state.epoch != request.epoch {
        changes.push("definition".into());
    }

    changes
}
