//! The dashboard's auto-task listing and writes. Each calls the runtime
//! method the dashboard's handler called before.

use std::collections::BTreeMap;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::governance::authorization::{
    DASHBOARD_AUTO_TASK_CREATE, DASHBOARD_AUTO_TASK_DELETE, DASHBOARD_AUTO_TASK_MINT,
    DASHBOARD_AUTO_TASK_SCHEDULE, DASHBOARD_AUTO_TASK_TOGGLE,
};
use orbit_types::workflow::auto_task_tag;
use serde_json::{Value, json};

use super::projection::AutoTaskListingProfile;
use super::{
    AutoTaskScheduleError, AutoTaskScheduleOutcome, AutoTaskScheduleProfile, AutoTaskToggleError,
    AutoTaskToggleOutcome, AutoTaskToggleProfile,
};
use super::{DashboardWrite, DashboardWritten, Listing, operation};
use crate::app::exec::{DashboardOrigin, DashboardRow, Operation};
use crate::{Core, OrbitRuntime, Task};

pub(super) fn listing(
    runtime: &OrbitRuntime,
    include_inactive_plugins: bool,
) -> Result<Listing, OrbitError> {
    let now = Utc::now();
    let listing =
        super::listing::auto_task_listing_state_lenient(runtime, include_inactive_plugins, now);
    // One freshness pass for the request, with no task-body hydration, that
    // reads only the tasks carrying a listed definition's tag: the rest of
    // the workspace never leaves the index [ORB-21824]. The metadata listing
    // is newest first, exactly as the per-tag listing was.
    let tags = listing
        .rows
        .iter()
        .map(|row| (auto_task_tag(&row.listed.definition.name), Vec::new()))
        .collect::<BTreeMap<_, Vec<&Task>>>();
    let tasks = if tags.is_empty() {
        Ok(Vec::new())
    } else {
        runtime.list_task_metadata_with_any_tag(&tags.keys().cloned().collect())
    };
    let mut instances = tags;
    for task in tasks.as_ref().map_or(&[][..], Vec::as_slice) {
        for tag in &task.tags {
            if let Some(group) = instances.get_mut(tag) {
                group.push(task);
            }
        }
    }
    let definitions = listing
        .rows
        .iter()
        .map(|row| {
            super::projection::project_auto_task_listing_row(
                runtime,
                row,
                AutoTaskListingProfile::Http {
                    minted: tasks.as_ref().map(|_| {
                        instances
                            .get(&auto_task_tag(&row.listed.definition.name))
                            .map_or(&[][..], Vec::as_slice)
                    }),
                    now,
                },
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let role_refusal = runtime
        .stored()
        .task()
        .writer()
        .err()
        .map(|error| error.to_string());
    Ok(Listing {
        definitions,
        inactive_plugin_count: listing.inactive_plugin_count,
        cursor_state_error: listing.cursor_state_error,
        load_errors: listing.load_errors,
        role_refusal,
    })
}

/// The governed operation one write requires and the audit row it declares.
pub(super) fn operation(origin: &DashboardOrigin, write: &DashboardWrite) -> Operation {
    let (governed, target, arguments) = match write {
        DashboardWrite::Create { fields, enabled } => (
            &DASHBOARD_AUTO_TASK_CREATE,
            fields
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or_default()
                .to_owned(),
            json!({
                "enabled": enabled,
                "fields_blake3": blake3::hash(fields.to_string().as_bytes())
                    .to_hex()
                    .to_string(),
            }),
        ),
        DashboardWrite::Toggle {
            name,
            expected_enabled,
            enabled,
        } => (
            &DASHBOARD_AUTO_TASK_TOGGLE,
            name.clone(),
            json!({"expected_enabled": expected_enabled, "enabled": enabled}),
        ),
        DashboardWrite::Mint { name } => (
            &DASHBOARD_AUTO_TASK_MINT,
            name.clone(),
            json!({"acknowledge_unconditional": true}),
        ),
        DashboardWrite::Schedule { name, schedule, .. } => (
            &DASHBOARD_AUTO_TASK_SCHEDULE,
            name.clone(),
            json!({"schedule": schedule}),
        ),
        DashboardWrite::UndecodedSchedule { name, arguments } => (
            &DASHBOARD_AUTO_TASK_SCHEDULE,
            name.clone(),
            arguments.clone(),
        ),
        DashboardWrite::Delete(params) => (
            &DASHBOARD_AUTO_TASK_DELETE,
            params.name.clone(),
            json!({"reason": params.reason, "force": params.force}),
        ),
    };
    Operation::dashboard_read("auto-task", "dashboard")
        .requires(governed)
        .dashboard_row(governed, target, arguments, origin.clone())
}

/// Run one admitted write.
pub(super) fn write(core: &Core, write: DashboardWrite) -> Result<DashboardWritten, OrbitError> {
    let runtime = core.runtime();
    Ok(match write {
        DashboardWrite::Create { fields, enabled } => {
            DashboardWritten::Created(operation::create_from_fields(runtime, fields, enabled))
        }
        DashboardWrite::Toggle {
            name,
            expected_enabled,
            enabled,
        } => DashboardWritten::Toggled(super::toggle::auto_task_toggle_command(
            runtime,
            &name,
            enabled,
            AutoTaskToggleProfile::Observed { expected_enabled },
        )),
        DashboardWrite::Mint { name } => {
            DashboardWritten::Minted(super::crud::auto_task_mint(runtime, &name).map(Box::new))
        }
        DashboardWrite::Schedule {
            name,
            expected_schedule,
            schedule,
        } => DashboardWritten::Scheduled(super::schedule_edit::auto_task_schedule_command(
            runtime,
            &name,
            *schedule,
            AutoTaskScheduleProfile::Observed { expected_schedule },
        )),
        DashboardWrite::UndecodedSchedule { .. } => DashboardWritten::ScheduleAdmitted,
        DashboardWrite::Delete(params) => DashboardWritten::Deleted(
            super::delete::auto_task_delete_command(runtime, params).map(Box::new),
        ),
    })
}

/// The audit row an admitted write owes; a write that failed outright owes none.
pub(super) fn row(
    result: Result<&DashboardWritten, &OrbitError>,
    arguments: Value,
) -> Option<DashboardRow> {
    recorded(result.ok()?, arguments)
}

/// The audit row an admitted write owes. A toggle or schedule edit that
/// wrote nothing (already set, stale, unknown name, unreadable definition)
/// owes none.
fn recorded(written: &DashboardWritten, mut arguments: Value) -> Option<DashboardRow> {
    let mut target = None;
    let failure = match written {
        DashboardWritten::Created(result) => result.as_ref().err().map(ToString::to_string),
        DashboardWritten::Toggled(Ok(AutoTaskToggleOutcome::Written(_)))
        | DashboardWritten::Scheduled(Ok(AutoTaskScheduleOutcome::Written(_))) => None,
        DashboardWritten::Toggled(Err(AutoTaskToggleError::Write(error)))
        | DashboardWritten::Scheduled(Err(AutoTaskScheduleError::Write(error))) => {
            Some(error.to_string())
        }
        DashboardWritten::Toggled(_)
        | DashboardWritten::Scheduled(_)
        | DashboardWritten::ScheduleAdmitted => return None,
        DashboardWritten::Minted(Ok(task)) => {
            arguments["task_id"] = json!(task.id.to_string());
            None
        }
        DashboardWritten::Minted(Err(error)) => Some(error.to_string()),
        DashboardWritten::Deleted(Ok(report)) => {
            target = Some(report.name.clone());
            None
        }
        DashboardWritten::Deleted(Err(error)) => Some(error.to_string()),
    };
    Some(DashboardRow {
        failure,
        target,
        arguments,
    })
}
