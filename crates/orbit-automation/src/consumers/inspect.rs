//! Read-only diagnostics share persisted scheduler state; inspection never ticks.

use crate::delivery;
use crate::host::AutomationHost;
use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::workflow::{
    AutoTaskDefinition, AutoTaskSchedule, DedupePolicy, RoutineDefinition,
    automation::{
        AutomationDiagnostic, AutomationState, BatchState, DeliveryOwnership, DeliveryTrigger,
    },
};

use super::ownership;

pub fn inspect_auto_task<H: AutomationHost>(
    host: &H,
    definition: &AutoTaskDefinition,
    now: DateTime<Utc>,
) -> Result<AutomationDiagnostic, OrbitError> {
    let AutoTaskSchedule::Deliveries {
        deliveries_landed: declared,
    } = &definition.schedule
    else {
        return Err(OrbitError::InvalidInput("not a delivery definition".into()));
    };

    let ownership = ownership::resolve(host, declared.owner_machine.as_deref());
    let trigger = ownership::with_resolved_owner(declared, &ownership);
    let epoch = ownership::auto_task_epoch(definition, &trigger)?;

    let admission_deferred = matches!(definition.dedupe, DedupePolicy::SkipIfOpen)
        && super::auto_task_admission_deferral(host, definition)?.is_some();

    inspect(
        host,
        Inspection {
            kind: "auto-task",
            name: &definition.name,
            epoch: &epoch,
            trigger: &trigger,
            ownership,
            enabled: definition.enabled,
            admission_deferred,
        },
        now,
    )
}

pub fn inspect_routine<H: AutomationHost>(
    host: &H,
    definition: &RoutineDefinition,
    now: DateTime<Utc>,
) -> Result<AutomationDiagnostic, OrbitError> {
    if definition.trigger.state.is_some() {
        return super::members::evaluate(host, definition, true, now);
    }

    let declared = definition
        .trigger
        .deliveries_landed
        .as_ref()
        .ok_or_else(|| OrbitError::InvalidInput("not a delivery routine".into()))?;

    let ownership = ownership::resolve(host, declared.owner_machine.as_deref());
    let mut effective_trigger = ownership::with_resolved_owner(declared, &ownership);
    let epoch = ownership::routine_epoch(definition, &effective_trigger)?;

    effective_trigger.retries = effective_trigger.retries.min(definition.policy.retries.max);

    inspect(
        host,
        Inspection {
            kind: "routine",
            name: &definition.name,
            epoch: &epoch,
            trigger: &effective_trigger,
            ownership,
            enabled: definition.enabled,
            admission_deferred: false,
        },
        now,
    )
}

/// One delivery consumer as inspection sees it, already resolved against the
/// same ownership and epoch the evaluator uses.
struct Inspection<'a> {
    kind: &'a str,
    name: &'a str,
    epoch: &'a str,
    trigger: &'a DeliveryTrigger,
    ownership: DeliveryOwnership,
    enabled: bool,
    admission_deferred: bool,
}

fn inspect<H: AutomationHost>(
    host: &H,
    request: Inspection<'_>,
    now: DateTime<Utc>,
) -> Result<AutomationDiagnostic, OrbitError> {
    let Inspection {
        kind,
        name,
        epoch,
        trigger,
        ownership,
        enabled,
        admission_deferred,
    } = request;

    let consumer = super::consumer_key(host, kind, name)?;
    let store = host.automation_store()?;
    let state = store.automation_state(&consumer)?;

    let definition_changed = state
        .as_ref()
        .is_some_and(|state| state.epoch != epoch || state.branch != trigger.branch);

    // Mirrors the evaluator's precedence without advancing any state. An
    // edited definition comes first: it has to be restored before any owner
    // question matters.
    let reason = if definition_changed {
        delivery::DEFINITION_CHANGED
    } else if !enabled {
        "disabled"
    } else if let Some(refusal) = ownership.refusal() {
        refusal
    } else {
        match &state {
            None => "awaiting_baseline",
            Some(state) => scheduling_reason(state, trigger, admission_deferred, now),
        }
    };

    Ok(AutomationDiagnostic {
        reason: reason.into(),
        state,
        ownership: Some(ownership),
        waivers: store.automation_waivers(&consumer, 20)?,
        receipts: store
            .automation_receipts(&consumer, 20)?
            .into_iter()
            .map(Into::into)
            .collect(),
    })
}

/// Why a baselined consumer owned here is or is not due, read from persisted
/// state alone.
fn scheduling_reason(
    state: &AutomationState,
    trigger: &DeliveryTrigger,
    admission_deferred: bool,
    now: DateTime<Utc>,
) -> &'static str {
    let active_is_settled = state
        .active
        .as_ref()
        .is_some_and(|active| matches!(active.state, BatchState::Exhausted | BatchState::Failed));
    let claim_awaiting_admission = state
        .active
        .as_ref()
        .filter(|active| active.action_id.is_none() && active.state == BatchState::Claimed);
    let threshold_reached = state.pending.len() >= trigger.threshold;
    let oldest_waited_out = state.pending.first().is_some_and(|oldest| {
        now.signed_duration_since(oldest.landed_at).num_minutes()
            >= i64::from(trigger.max_wait_minutes)
    });

    if active_is_settled {
        "needs_attention"
    } else if claim_awaiting_admission
        .is_some_and(|active| active.attempt > 1 && now > active.deadline())
    {
        "retry_deadline_expired"
    } else if claim_awaiting_admission
        .is_some_and(|active| active.retry_after.is_some_and(|at| now < at))
    {
        "retry_backoff"
    } else if state.active.is_some() {
        "batch_pending"
    } else if (threshold_reached || oldest_waited_out) && admission_deferred {
        "open_instance"
    } else if threshold_reached {
        "threshold_reached"
    } else if oldest_waited_out {
        "max_wait_reached"
    } else if !state.unresolved.is_empty() {
        "evidence_unavailable"
    } else {
        "not_due"
    }
}
