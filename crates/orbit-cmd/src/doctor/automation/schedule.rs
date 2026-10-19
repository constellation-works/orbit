//! Scheduling health: enabled cron routines whose last success has gone stale,
//! and a host shutdown or reboot that holds unattended admissions.

use super::pull::format_age;
use crate::doctor::*;

/// Warn when an enabled cron routine's last success is older than three times
/// its schedule interval. A routine that has never succeeded is judged from
/// the cursor baseline. Disabled, paused, and non-cron routines are quiet.
pub(in crate::doctor) fn doctor_check_routine_freshness(
    runtime: &OrbitRuntime,
) -> WorkspaceDoctorResult {
    const CHECK: &str = "routine-freshness";
    let root = runtime.global_root();
    match orbit_registry::inspect_machine_identity(&root) {
        Ok(orbit_registry::MachineIdentityState::Absent) => check(
            CHECK,
            WorkspaceDoctorStatus::Skipped,
            "no machine identity; run `orbit init`".to_string(),
        ),
        Err(error) => check(
            CHECK,
            WorkspaceDoctorStatus::Warning,
            format!("cannot inspect machine identity: {error}"),
        ),
        Ok(orbit_registry::MachineIdentityState::Present(_)) => match stale_routine_message(&root) {
            Ok(None) => check(
                CHECK,
                WorkspaceDoctorStatus::Ok,
                "no enabled cron routine is older than 3× its schedule interval".to_string(),
            ),
            Ok(Some(message)) => actionable_check(
                CHECK,
                WorkspaceDoctorStatus::Warning,
                message,
                "Inspect the named routine with `orbit routine list` and run `orbit clock tick`."
                    .to_string(),
            ),
            Err(error) => check(
                CHECK,
                WorkspaceDoctorStatus::Warning,
                format!("cannot judge routine freshness: {error}"),
            ),
        },
    }
}

/// `None` when every enabled cron routine is inside the window.
fn stale_routine_message(global_root: &std::path::Path) -> Result<Option<String>, OrbitError> {
    use orbit_core::application::routines::{minimum_cron_interval, parse_cron};

    let report = crate::registry_routines::routine_statuses(global_root)?;
    let database =
        orbit_config::resolved_audit_db_path(&orbit_config::ConfigRoots::global_only(global_root))?;
    if !database.exists() {
        return Ok(None);
    }
    let store = orbit_store::Store::open_read_only(&database)?;
    let now = chrono::Utc::now();
    let mut stale = Vec::new();
    for status in &report.statuses {
        if !status.effective() {
            continue;
        }
        let expression = status.routine.definition.trigger.cron.as_str();
        if expression.is_empty() {
            continue;
        }
        let Some(baseline) = status.first_observed_at.as_deref() else {
            continue;
        };
        let name = status.routine.definition.name.as_str();
        let cron = parse_cron(expression)?;
        let interval = minimum_cron_interval(&cron, now)?;
        let Some(limit) = interval.checked_mul(3) else {
            continue;
        };
        let success = store.routine_latest_succeeded_fire(name)?;
        let reference = match &success {
            Some(fire) => parse_doctor_time(&fire.updated_at)?,
            None => parse_doctor_time(baseline)?,
        };
        let age = now.signed_duration_since(reference);
        if age <= limit {
            continue;
        }
        let age = format_age(age);
        let detail = if success.is_some() {
            format!("last succeeded {age} ago, older than 3× its schedule interval")
        } else {
            format!(
                "has not succeeded since it was first observed {age} ago, older than 3× its \
                 schedule interval"
            )
        };
        stale.push(format!("routine '{name}' {detail}"));
    }
    stale.sort();
    if stale.is_empty() {
        Ok(None)
    } else {
        Ok(Some(stale.join("; ")))
    }
}

fn parse_doctor_time(raw: &str) -> Result<chrono::DateTime<chrono::Utc>, OrbitError> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .map(|value| value.with_timezone(&chrono::Utc))
        .map_err(|error| OrbitError::Store(format!("invalid stored timestamp '{raw}': {error}")))
}

/// A host shutdown inside the hold horizon holds every unattended admission —
/// routine and auto-task fires, drain waves, the ship sweep — so doctor warns
/// and names the mode and time [ORB-12968]. A later schedule is still named,
/// as ok: nothing is wrong, and work continues until the horizon. Nothing is
/// wrong with the workspace either way.
pub(in crate::doctor) fn doctor_check_host_shutdown(
    runtime: &OrbitRuntime,
) -> WorkspaceDoctorResult {
    let Some(shutdown) = runtime.scheduled_host_shutdown() else {
        return check(
            "host-shutdown",
            WorkspaceDoctorStatus::Ok,
            "no host shutdown or reboot is scheduled".to_string(),
        );
    };
    if runtime.host_shutdown_admission_hold().is_some() {
        return actionable_check(
            "host-shutdown",
            WorkspaceDoctorStatus::Warning,
            format!(
                "{} (read from {}); scheduled routines, auto-tasks, drain waves and the ship \
                 sweep start no new runs until it clears, and in-flight runs are not touched",
                shutdown.describe(),
                shutdown.source
            ),
            "Nothing to repair: admissions resume on their own after the restart, or once the \
             schedule is further away than machine.host_shutdown_hold_minutes. To resume them \
             now, cancel the schedule as root with `shutdown -c`."
                .to_string(),
        );
    }
    check(
        "host-shutdown",
        WorkspaceDoctorStatus::Ok,
        format!(
            "{} (read from {}); admissions continue until machine.host_shutdown_hold_minutes \
             of that time, and in-flight runs are not touched",
            shutdown.describe(),
            shutdown.source
        ),
    )
}
