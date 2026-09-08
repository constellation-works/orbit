//! Auto-task due computation [ORB-10149]: given a schedule, the cursor's
//! lower bound, and "now", decide whether a definition fires this pass and for
//! which scheduled slot.
//!
//! Catch-up always collapses: fires missed while the host was down produce a
//! single make-up task, not one per missed slot. Cron schedules reuse the
//! routine due-math (`crate::routines::due`) under [`MissedRunPolicy::CatchUpOnce`];
//! interval schedules fire at most one task for the most recent boundary.
//!
//! [`decide_due`] and [`next_scheduled_slot`] answer different questions and
//! are expected to disagree. The first is catch-up eligibility — it can name a
//! slot already in the past that the scheduler still owes a fire. The second is
//! the next scheduled occurrence, always strictly ahead of `now`, and is what
//! operator surfaces render as "next evaluation". Both derive every slot from
//! the arithmetic in this module so the two views stay anchored identically.

use chrono::{DateTime, Duration, Local, Utc};
use orbit_common::OrbitError;
use orbit_types::workflow::{AutoTaskSchedule, MAX_AUTO_TASK_INTERVAL_MINUTES, MissedRunPolicy};

use crate::routines::due::{DueDecision, due_decision, next_occurrence, parse_cron};

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
        AutoTaskSchedule::Interval { every_minutes } => interval_period(*every_minutes).map(drop),
    }
}

/// Decide whether a definition is due.
///
/// `baseline` is the first-observed slot recorded on registration; `last_slot`
/// is the most recently consumed slot when the definition has fired before.
/// The effective exclusive floor is `last_slot` when present, otherwise
/// `baseline` — a definition never fires for slots predating its registration.
///
/// The returned slot may already be in the past: that is a catch-up fire the
/// scheduler still owes, not a prediction. Use [`next_scheduled_slot`] for the
/// forward-looking projection.
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

/// Project the schedule's next occurrence, strictly after `now`.
///
/// This is the canonical answer for "when does this definition next come
/// around?" — the one operator surfaces render. It never reports a pending
/// catch-up slot, which by definition already passed; ask [`decide_due`] for
/// that.
///
/// `baseline` is the cursor's first-observed slot. Interval schedules are
/// anchored to it and cannot be projected without one; cron schedules are
/// absolute, so they project with or without a cursor. Delivery schedules have
/// no time-based occurrence at all and always project `None`.
pub fn next_scheduled_slot(
    schedule: &AutoTaskSchedule,
    baseline: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>, OrbitError> {
    match schedule {
        AutoTaskSchedule::Deliveries { .. } => Ok(None),
        AutoTaskSchedule::Cron { cron } => {
            // Cron is evaluated in host-local time, as the due path does; the
            // projection is reported in UTC like every other stored slot.
            let cron = parse_cron(cron)?;
            let next = next_occurrence(&cron, &now.with_timezone(&Local))?;
            Ok(Some(next.with_timezone(&Utc)))
        }
        AutoTaskSchedule::Interval { every_minutes } => baseline
            .map(|baseline| next_interval_slot(*every_minutes, baseline, now))
            .transpose(),
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
    let period = interval_period(every_minutes)?;
    let Some(latest_slot) = interval_slot_at_or_before(period, baseline, now)? else {
        return Ok(AutoTaskDueDecision::NotDue);
    };

    if latest_slot > lower_bound {
        Ok(AutoTaskDueDecision::Fire {
            slot: latest_slot.to_rfc3339(),
        })
    } else {
        Ok(AutoTaskDueDecision::NotDue)
    }
}

/// The first interval boundary strictly after `now`.
///
/// The boundary `now` falls in has already been offered to the scheduler, and
/// the baseline boundary itself is the exclusive due floor that never fires, so
/// the projection is always one period past whichever of those applies.
fn next_interval_slot(
    every_minutes: u64,
    baseline: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<DateTime<Utc>, OrbitError> {
    let period = interval_period(every_minutes)?;
    let current = interval_slot_at_or_before(period, baseline, now)?.unwrap_or(baseline);

    interval_slot(current, period, 1)
}

/// The interval period as a duration, rejecting anything outside the supported
/// range before slot arithmetic runs. This is the single range rule: validation
/// and both slot calculations go through it, so an interval that CRUD rejects
/// can never reach the arithmetic below.
fn interval_period(every_minutes: u64) -> Result<Duration, OrbitError> {
    if !(1..=MAX_AUTO_TASK_INTERVAL_MINUTES).contains(&every_minutes) {
        return Err(interval_range_error());
    }

    i64::try_from(every_minutes)
        .ok()
        .and_then(Duration::try_minutes)
        .ok_or_else(interval_range_error)
}

/// The most recent interval boundary at or before `now`, or `None` when `now`
/// precedes the anchor and no boundary has arrived yet.
///
/// Jumping straight to the latest boundary is what collapses a downtime gap
/// into one catch-up fire instead of one fire per missed boundary.
fn interval_slot_at_or_before(
    period: Duration,
    baseline: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>, OrbitError> {
    if now < baseline {
        return Ok(None);
    }

    let periods = now.signed_duration_since(baseline).num_minutes() / period.num_minutes();

    interval_slot(baseline, period, periods).map(Some)
}

/// `anchor + periods · period`, refusing to wrap or saturate. A cursor holding
/// an extreme baseline must surface an error, never a silently wrapped slot
/// that the scheduler would treat as due.
fn interval_slot(
    anchor: DateTime<Utc>,
    period: Duration,
    periods: i64,
) -> Result<DateTime<Utc>, OrbitError> {
    period
        .num_minutes()
        .checked_mul(periods)
        .and_then(Duration::try_minutes)
        .and_then(|offset| anchor.checked_add_signed(offset))
        .ok_or_else(|| {
            OrbitError::InvalidInput(
                "auto-task interval slot falls outside the representable date range".to_string(),
            )
        })
}

fn interval_range_error() -> OrbitError {
    OrbitError::InvalidInput(format!(
        "auto-task interval every_minutes must be between 1 and {MAX_AUTO_TASK_INTERVAL_MINUTES}"
    ))
}
