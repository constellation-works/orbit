//! The dashboard's routine and clock writes. Each calls the routine edit or
//! clock control the dashboard's handler called before.

use orbit_common::OrbitError;
use orbit_common::governance::authorization::{
    DASHBOARD_CLOCK_CADENCE, DASHBOARD_CLOCK_SERVICE, DASHBOARD_ROUTINE_BREAKER,
    DASHBOARD_ROUTINE_TOGGLE, DASHBOARD_ROUTINE_TRIGGER,
};
use serde_json::{Value, json};

use super::{ClockAction, ClockControl, ClockControlled, DashboardWrite, DashboardWritten};
use crate::Core;
use crate::app::exec::{DashboardOrigin, DashboardRow, Operation};
use crate::application::routines::{
    RoutineEditRefusal, control_listed_routine_breaker, edit_listed_routine_trigger,
    set_clock_cadence, set_clock_enabled, toggle_listed_routine,
};

/// The governed operation one write requires and the audit row it declares.
pub(super) fn operation(origin: &DashboardOrigin, write: &DashboardWrite) -> Operation {
    let (governed, target, arguments) = match write {
        DashboardWrite::Toggle {
            routine, enabled, ..
        } => (
            &DASHBOARD_ROUTINE_TOGGLE,
            routine.name.clone(),
            json!({"source": routine.source, "target": routine.target, "enabled": enabled}),
        ),
        DashboardWrite::Trigger {
            routine, trigger, ..
        } => (
            &DASHBOARD_ROUTINE_TRIGGER,
            routine.name.clone(),
            json!({"source": routine.source, "target": routine.target, "trigger": trigger}),
        ),
        DashboardWrite::UndecodedTrigger { name, arguments } => {
            (&DASHBOARD_ROUTINE_TRIGGER, name.clone(), arguments.clone())
        }
        DashboardWrite::Clock(control) => (
            match control.action {
                ClockAction::Enable | ClockAction::Disable => &DASHBOARD_CLOCK_SERVICE,
                ClockAction::SetCadence => &DASHBOARD_CLOCK_CADENCE,
            },
            "clock".to_string(),
            json!({"action": control.action, "cadence_seconds": control.cadence_seconds}),
        ),
        DashboardWrite::Breaker {
            name,
            source,
            action,
            ..
        } => (
            &DASHBOARD_ROUTINE_BREAKER,
            name.clone(),
            json!({"source": source, "action": action.as_str()}),
        ),
    };
    Operation::dashboard_read("routine", "dashboard")
        .requires(governed)
        .dashboard_row(governed, target, arguments, origin.clone())
}

/// Run one admitted write.
pub(super) fn write(
    core: &Core,
    machine_name: &str,
    write: DashboardWrite,
) -> Result<DashboardWritten, OrbitError> {
    let shared_root = core.runtime().shared_root();
    Ok(match write {
        DashboardWrite::Toggle {
            routine,
            expected_enabled,
            enabled,
            statuses,
        } => {
            let report = statuses()?;
            DashboardWritten::Edited(toggle_listed_routine(
                &report,
                &routine.name,
                Some((&routine.source, &shared_root)),
                &routine.target,
                expected_enabled,
                enabled,
            ))
        }
        DashboardWrite::Trigger {
            routine,
            expected,
            trigger,
            statuses,
        } => {
            let report = statuses()?;
            DashboardWritten::Edited(edit_listed_routine_trigger(
                &report,
                &routine.name,
                Some((&routine.source, &shared_root)),
                &routine.target,
                &expected,
                *trigger,
            ))
        }
        DashboardWrite::UndecodedTrigger { .. } => DashboardWritten::TriggerAdmitted,
        DashboardWrite::Clock(control) => {
            DashboardWritten::Clock(control_clock(control, machine_name)?)
        }
        DashboardWrite::Breaker {
            name,
            source,
            action,
            global_root,
            opener,
            statuses,
        } => DashboardWritten::Breaker(statuses().and_then(|report| {
            // The request names the source workspace but not its `.orbit`
            // directory.
            control_listed_routine_breaker(
                opener,
                &global_root,
                &report,
                &name,
                Some((&source, None)),
                action,
                "dashboard",
            )
        })),
    })
}

/// The audit row an admitted write owes: only a write that was attempted.
pub(super) fn row(
    result: Result<&DashboardWritten, &OrbitError>,
    target: &str,
    arguments: Value,
) -> Option<DashboardRow> {
    let failure = match result.ok()? {
        DashboardWritten::Breaker(Ok(Ok(outcome))) => (!outcome.changed())
            .then(|| format!("the breaker of '{target}' is not tripped on this host")),
        // An unlisted routine has nothing to audit.
        DashboardWritten::Breaker(Ok(Err(RoutineEditRefusal::NotFound { .. }))) => {
            return None;
        }
        DashboardWritten::Breaker(Ok(Err(refusal))) => {
            Some(format!("{}: {}", refusal.code(), refusal.message()))
        }
        DashboardWritten::Breaker(Err(_)) => Some("routine breaker update failed".to_string()),
        DashboardWritten::Edited(Ok(Ok(_))) => None,
        // An unlisted routine has nothing to audit.
        DashboardWritten::Edited(Ok(Err(RoutineEditRefusal::NotFound { .. }))) => {
            return None;
        }
        // Every other refusal (owner authority, workspace, stale
        // state, retargeted definition) wrote nothing; audit it.
        DashboardWritten::Edited(Ok(Err(refusal))) => {
            Some(format!("{}: {}", refusal.code(), refusal.message()))
        }
        DashboardWritten::Edited(Err(error)) => Some(error.to_string()),
        DashboardWritten::TriggerAdmitted
        | DashboardWritten::Clock(
            ClockControlled::MachineMismatch { .. } | ClockControlled::Stale(_),
        ) => return None,
        DashboardWritten::Clock(ClockControlled::Applied { result, .. }) => {
            result.as_ref().err().map(ToString::to_string)
        }
    };
    Some(DashboardRow {
        failure,
        target: None,
        arguments,
    })
}

/// Change the clock only from the machine and state the browser observed.
fn control_clock(control: ClockControl, machine_name: &str) -> Result<ClockControlled, OrbitError> {
    let ClockControl {
        action,
        cadence_seconds,
        expected_enabled,
        expected_cadence_seconds,
        global_root,
        observe,
        stand_in,
    } = control;
    let (before, report) = observe()?;
    if machine_name != report.machine_name {
        return Ok(ClockControlled::MachineMismatch {
            machine_name: report.machine_name,
        });
    }
    if before.enabled != expected_enabled
        || before.configured_cadence_seconds != expected_cadence_seconds
    {
        return Ok(ClockControlled::Stale(Box::new(before)));
    }
    let result = stand_in
        .and_then(|stand_in| stand_in())
        .unwrap_or_else(|| match action {
            ClockAction::Enable => set_clock_enabled(&global_root, true).map(|_| ()),
            ClockAction::Disable => set_clock_enabled(&global_root, false).map(|_| ()),
            ClockAction::SetCadence => cadence_seconds
                .ok_or_else(|| {
                    OrbitError::InvalidInput(
                        "cadence_seconds is required for set_cadence".to_string(),
                    )
                })
                .and_then(|cadence| set_clock_cadence(&global_root, cadence).map(|_| ())),
        });
    Ok(ClockControlled::Applied {
        before: Box::new(before),
        result,
    })
}
