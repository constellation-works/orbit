//! Auto-task due computation [ORB-10149]: given a schedule, the cursor's
//! lower bound, and "now", decide whether a definition fires this pass and for
//! which scheduled slot.
//!
//! Catch-up always collapses: fires missed while the host was down produce a
//! single make-up task, not one per missed slot. Cron schedules reuse the
//! routine due-math (`crate::routines::due`) under [`MissedRunPolicy::CatchUpOnce`];
//! interval schedules fire at most one task for the most recent boundary.

use chrono::{DateTime, Duration, Local, Utc};
use orbit_common::OrbitError;
use orbit_types::workflow::{AutoTaskSchedule, MAX_AUTO_TASK_INTERVAL_MINUTES, MissedRunPolicy};

use crate::routines::due::{DueDecision, due_decision, parse_cron};

/// Outcome of the due check for one definition on one scheduler pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoTaskDueDecision {
    /// Nothing to do this pass.
    NotDue,
    /// Fire for `slot` (RFC 3339, UTC) — the idempotency key.
    Fire { slot: String },
}

/// Validate a schedule fail-closed: a cron form must parse as a 5-field cron,
/// an interval must be within the supported range. CRUD and the loader call this so a bad
/// schedule is rejected before it can silently never fire.
pub fn validate_schedule(schedule: &AutoTaskSchedule) -> Result<(), OrbitError> {
    match schedule {
        AutoTaskSchedule::Deliveries { deliveries_landed } => {
            deliveries_landed.validate().map_err(Into::into)
        }
        AutoTaskSchedule::Cron { cron } => {
            parse_cron(cron)?;
            Ok(())
        }
        AutoTaskSchedule::Interval { every_minutes }
            if *every_minutes == 0 || *every_minutes > MAX_AUTO_TASK_INTERVAL_MINUTES =>
        {
            Err(OrbitError::InvalidInput(format!(
                "auto-task interval every_minutes must be between 1 and {MAX_AUTO_TASK_INTERVAL_MINUTES}"
            )))
        }
        AutoTaskSchedule::Interval { .. } => Ok(()),
    }
}

/// Decide whether a definition is due.
///
/// `baseline` is the first-observed slot recorded on registration; `last_slot`
/// is the most recently consumed slot when the definition has fired before.
/// The effective exclusive floor is `last_slot` when present, otherwise
/// `baseline` — a definition never fires for slots predating its registration.
pub fn decide_due(
    schedule: &AutoTaskSchedule,
    baseline: DateTime<Utc>,
    last_slot: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Result<AutoTaskDueDecision, OrbitError> {
    let lower_bound = last_slot.unwrap_or(baseline);
    match schedule {
        AutoTaskSchedule::Deliveries { .. } => Err(OrbitError::InvalidInput(
            "delivery schedules require source evaluation".into(),
        )),
        AutoTaskSchedule::Cron { cron } => decide_cron(cron, lower_bound, now),
        AutoTaskSchedule::Interval { every_minutes } => {
            decide_interval(*every_minutes, baseline, lower_bound, now)
        }
    }
}

fn decide_cron(
    cron: &str,
    lower_bound: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<AutoTaskDueDecision, OrbitError> {
    let cron = parse_cron(cron)?;
    // Cron is evaluated in host-local time (as routines do); the cursor is
    // stored in UTC, so translate the bound and `now` into Local for the
    // shared due-math and translate the resulting slot back to UTC.
    let lower_bound_local = lower_bound.with_timezone(&Local);
    let now_local = now.with_timezone(&Local);
    match due_decision(
        &cron,
        MissedRunPolicy::CatchUpOnce,
        &lower_bound_local,
        &now_local,
    )? {
        DueDecision::Fire { slot, .. } => Ok(AutoTaskDueDecision::Fire {
            slot: slot.with_timezone(&Utc).to_rfc3339(),
        }),
        DueDecision::NotDue => Ok(AutoTaskDueDecision::NotDue),
    }
}

fn decide_interval(
    every_minutes: u64,
    baseline: DateTime<Utc>,
    lower_bound: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<AutoTaskDueDecision, OrbitError> {
    let every_minutes = i64::try_from(every_minutes).map_err(|_| {
        OrbitError::InvalidInput(
            "auto-task interval every_minutes exceeds the supported signed duration".to_string(),
        )
    })?;
    if every_minutes == 0 {
        return Err(OrbitError::InvalidInput(
            "auto-task interval every_minutes must be at least 1".to_string(),
        ));
    }
    if now < baseline {
        return Ok(AutoTaskDueDecision::NotDue);
    }
    // Most recent interval boundary at or before `now`, anchored at baseline.
    // A single fire covers however many boundaries fell in a downtime gap
    // (catch-up collapse), because we jump straight to the latest boundary.
    let elapsed_minutes = now.signed_duration_since(baseline).num_minutes();
    let periods = elapsed_minutes / every_minutes;
    let latest_slot = baseline + Duration::minutes(every_minutes * periods);
    if latest_slot > lower_bound {
        Ok(AutoTaskDueDecision::Fire {
            slot: latest_slot.to_rfc3339(),
        })
    } else {
        Ok(AutoTaskDueDecision::NotDue)
    }
}
