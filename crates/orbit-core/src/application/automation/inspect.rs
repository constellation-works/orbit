//! Read-only diagnostics share persisted scheduler state; inspection never ticks.
use crate::OrbitRuntime;
use chrono::{DateTime, Utc};
use orbit_automation::delivery;
use orbit_common::OrbitError;
use orbit_types::workflow::{
    AutoTaskDefinition, AutoTaskSchedule, DedupePolicy, RoutineDefinition,
    automation::{AutomationDiagnostic, BatchState, DeliveryTrigger},
};

pub fn inspect_auto_task(
    runtime: &OrbitRuntime,
    definition: &AutoTaskDefinition,
    now: DateTime<Utc>,
) -> Result<AutomationDiagnostic, OrbitError> {
    let AutoTaskSchedule::Deliveries { deliveries_landed } = &definition.schedule else {
        return Err(OrbitError::InvalidInput("not a delivery definition".into()));
    };
    let epoch = delivery::definition_epoch(&(
        &definition.schedule,
        &definition.template,
        definition.dedupe,
    ))
    .map_err(orbit_automation::automation_error_to_orbit)?;
    let admission_deferred = matches!(definition.dedupe, DedupePolicy::SkipIfOpen)
        && super::auto_task_admission_deferral(runtime, definition)?.is_some();
    inspect(
        runtime,
        "auto-task",
        &definition.name,
        &epoch,
        deliveries_landed,
        definition.enabled,
        admission_deferred,
        now,
    )
}

pub fn inspect_routine(
    runtime: &OrbitRuntime,
    definition: &RoutineDefinition,
    now: DateTime<Utc>,
) -> Result<AutomationDiagnostic, OrbitError> {
    let trigger = definition
        .trigger
        .deliveries_landed
        .as_ref()
        .ok_or_else(|| OrbitError::InvalidInput("not a delivery routine".into()))?;
    let mut effective_trigger = trigger.clone();
    effective_trigger.retries = effective_trigger.retries.min(definition.policy.retries.max);
    let epoch =
        delivery::definition_epoch(&(&definition.trigger, &definition.target, &definition.policy))
            .map_err(orbit_automation::automation_error_to_orbit)?;
    inspect(
        runtime,
        "routine",
        &definition.name,
        &epoch,
        &effective_trigger,
        definition.enabled,
        false,
        now,
    )
}

#[allow(clippy::too_many_arguments)]
fn inspect(
    runtime: &OrbitRuntime,
    kind: &str,
    name: &str,
    epoch: &str,
    trigger: &DeliveryTrigger,
    enabled: bool,
    admission_deferred: bool,
    now: DateTime<Utc>,
) -> Result<AutomationDiagnostic, OrbitError> {
    let consumer = super::consumer_key(runtime, kind, name)?;
    let store = runtime.automation_store()?;
    let state = store.automation_state(&consumer)?;
    let owner = trigger.owner_machine.as_deref();
    let owned_here =
        owner.is_some_and(|owner| Some(owner) == runtime.automation_machine_identity());
    let owned_elsewhere =
        owner.is_some_and(|owner| Some(owner) != runtime.automation_machine_identity());
    let reason = match &state {
        None => {
            if !enabled {
                "disabled"
            } else if owned_elsewhere {
                "owned_elsewhere"
            } else if !owned_here {
                "disabled"
            } else {
                "awaiting_baseline"
            }
        }
        Some(state) if state.epoch != epoch || state.branch != trigger.branch => {
            "definition_changed"
        }
        Some(_) if !enabled => "disabled",
        Some(_) if owned_elsewhere => "owned_elsewhere",
        Some(_) if !owned_here => "disabled",
        Some(state)
            if state
                .active
                .as_ref()
                .is_some_and(|a| matches!(a.state, BatchState::Exhausted | BatchState::Failed)) =>
        {
            "needs_attention"
        }
        Some(state)
            if state.active.as_ref().is_some_and(|active| {
                active.action_id.is_none()
                    && active.state == BatchState::Claimed
                    && active.attempt > 1
                    && now > active.batch.retry_until
            }) =>
        {
            "retry_deadline_expired"
        }
        Some(state)
            if state.active.as_ref().is_some_and(|active| {
                active.action_id.is_none()
                    && active.state == BatchState::Claimed
                    && active.retry_after.is_some_and(|at| now < at)
            }) =>
        {
            "retry_backoff"
        }
        Some(state) if state.active.is_some() => "batch_pending",
        Some(state) if state.pending.len() >= trigger.threshold && admission_deferred => {
            "open_instance"
        }
        Some(state) if state.pending.len() >= trigger.threshold => "threshold_reached",
        Some(state)
            if state.pending.first().is_some_and(|d| {
                now.signed_duration_since(d.landed_at).num_minutes()
                    >= i64::from(trigger.max_wait_minutes)
            }) =>
        {
            if admission_deferred {
                "open_instance"
            } else {
                "max_wait_reached"
            }
        }
        Some(state) if !state.unresolved.is_empty() => "evidence_unavailable",
        Some(_) => "not_due",
    };
    Ok(AutomationDiagnostic {
        reason: reason.into(),
        state,
        waivers: store.automation_waivers(&consumer, 20)?,
        receipts: store
            .automation_receipts(&consumer, 20)?
            .into_iter()
            .map(Into::into)
            .collect(),
    })
}
