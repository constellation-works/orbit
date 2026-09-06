//! Read-only diagnostics share persisted scheduler state; inspection never ticks.
use crate::OrbitRuntime;
use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::workflow::automation::{AutomationDiagnostic, DeliveryTrigger};

pub fn inspect(
    runtime: &OrbitRuntime,
    kind: &str,
    name: &str,
    trigger: &DeliveryTrigger,
    enabled: bool,
    now: DateTime<Utc>,
) -> Result<AutomationDiagnostic, OrbitError> {
    let consumer = super::consumer_key(runtime, kind, name)?;
    let store = runtime.automation_store()?;
    let state = store.automation_state(&consumer)?;
    let reason = match &state {
        _ if trigger.owner_machine.as_deref() != runtime.automation_machine_identity() => {
            "owned_elsewhere"
        }
        None => {
            if enabled {
                "awaiting_baseline"
            } else {
                "disabled"
            }
        }
        Some(_) if !enabled => "disabled",
        Some(state)
            if state.active.as_ref().is_some_and(|a| {
                matches!(
                    a.state,
                    orbit_types::workflow::automation::BatchState::Exhausted
                        | orbit_types::workflow::automation::BatchState::Failed
                )
            }) =>
        {
            "needs_attention"
        }
        Some(state) if state.active.is_some() => "batch_pending",
        Some(state) if state.pending.len() >= trigger.threshold => "threshold_reached",
        Some(state)
            if state.pending.first().is_some_and(|d| {
                now.signed_duration_since(d.landed_at).num_minutes()
                    >= i64::from(trigger.max_wait_minutes)
            }) =>
        {
            "max_wait_reached"
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
