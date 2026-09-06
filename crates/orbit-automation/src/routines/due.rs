//! Due computation for routines [ORB-10021]: given a cron trigger, the
//! host-local cursor, and "now", decide whether a routine fires this sweep
//! and for which scheduled slot.
//!
//! The computation is O(1) per routine — `find_previous_occurrence` from
//! `now`, compared against the cursor — never an iteration over every slot
//! in a gap, so a week of downtime against a minutely cron costs the same
//! as one minute.

use chrono::{DateTime, Duration, TimeZone, Timelike};
use croner::Cron;
use orbit_common::OrbitError;
use orbit_types::workflow::MissedRunPolicy;

/// How far past its scheduled slot a fire still counts as "natural" for
/// `missed_run: skip`. Two sweep intervals: tolerates one slow or skipped
/// sweep without reclassifying the slot as missed.
pub const NATURAL_SLOT_GRACE_SECONDS: i64 = 120;

/// Outcome of the due check for one routine on one sweep pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DueDecision<Tz: TimeZone> {
    /// Nothing to do this pass.
    NotDue,
    /// Fire for `slot`. `is_catch_up` marks a make-up fire for a slot that
    /// fell in a gap (only produced under `missed_run: catch_up_once`).
    Fire {
        /// The scheduled slot this fire consumes (the idempotency key).
        slot: DateTime<Tz>,
        /// Whether this is a make-up fire rather than a natural one.
        is_catch_up: bool,
    },
}

/// Pin a scheduled occurrence to its minute (seconds and sub-seconds
/// zeroed). Slot identity must be stable across sweeps for the idempotency
/// key to hold.
pub fn truncate_to_minute<Tz: TimeZone>(value: DateTime<Tz>) -> Result<DateTime<Tz>, OrbitError> {
    value
        .with_second(0)
        .and_then(|value| value.with_nanosecond(0))
        .ok_or_else(|| OrbitError::InvalidInput("timestamp cannot be minute-aligned".to_string()))
}

/// Parse and validate a routine cron expression (standard 5-field form,
/// evaluated in host-local time by the caller's choice of `Tz`).
pub fn parse_cron(expression: &str) -> Result<Cron, OrbitError> {
    expression.parse::<Cron>().map_err(|error| {
        OrbitError::InvalidInput(format!("invalid cron expression '{expression}': {error}"))
    })
}

/// Decide whether a routine is due.
///
/// `lower_bound` is the exclusive floor slots must be after: the cursor's
/// `last_slot` when one exists, otherwise the baseline (first observation) —
/// a routine never fires for slots that predate its registration on this
/// host.
pub fn due_decision<Tz: TimeZone>(
    cron: &Cron,
    missed_run: MissedRunPolicy,
    lower_bound: &DateTime<Tz>,
    now: &DateTime<Tz>,
) -> Result<DueDecision<Tz>, OrbitError> {
    // Latest scheduled slot at or before now.
    let previous = cron
        .find_previous_occurrence(now, true)
        .map_err(|error| OrbitError::InvalidInput(format!("cron evaluation failed: {error}")))?;
    // croner carries `now`'s sub-minute component into the occurrence it
    // returns, which would make the slot different on every sweep within the
    // same minute — breaking the (name, slot) idempotency key. 5-field cron
    // is minute-granular by definition, so pin slots to the minute.
    let previous = truncate_to_minute(previous)?;

    if previous <= *lower_bound {
        return Ok(DueDecision::NotDue);
    }

    let age = now.clone().signed_duration_since(previous.clone());
    let natural = age <= Duration::seconds(NATURAL_SLOT_GRACE_SECONDS);
    if natural {
        return Ok(DueDecision::Fire {
            slot: previous,
            is_catch_up: false,
        });
    }

    match missed_run {
        // One make-up fire for the latest missed slot, no matter how many
        // slots the gap swallowed ("collapses history" — see 2_design.md §6).
        MissedRunPolicy::CatchUpOnce => Ok(DueDecision::Fire {
            slot: previous,
            is_catch_up: true,
        }),
        // Wait for the next natural slot. The cursor is left untouched:
        // correctness only needs slots to be after `lower_bound`, so an
        // unconsumed missed slot simply never fires.
        MissedRunPolicy::Skip => Ok(DueDecision::NotDue),
    }
}
