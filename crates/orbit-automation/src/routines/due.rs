//! Due computation for routines [ORB-10021]: given a cron trigger, the
//! host-local cursor, and "now", decide whether a routine fires this sweep
//! and for which scheduled slot.
//!
//! The computation is O(1) per routine — `find_previous_occurrence` from
//! `now`, compared against the cursor — never an iteration over every slot
//! in a gap, so a week of downtime against a minutely cron costs the same
//! as one minute.
//!
//! Two different questions live here and must not be conflated. [`due_decision`]
//! answers *catch-up eligibility*: "is there an unconsumed slot this sweep may
//! fire?", and under `catch_up_once` that answer is a slot already in the past.
//! [`next_occurrence`] answers *the next scheduled occurrence*: "when does this
//! cron next come around?", always strictly ahead of `now`. Operator surfaces
//! render the second; the scheduler acts on the first. They disagree exactly
//! when a missed slot is pending, and that disagreement is correct.

use chrono::{DateTime, Duration, TimeZone, Timelike};
use croner::Cron;
use orbit_common::OrbitError;
use orbit_types::workflow::MissedRunPolicy;

/// How far past its scheduled slot a fire still counts as "natural" for
/// `missed_run: skip` at the default 60-second sweep cadence. Two sweep
/// intervals tolerate one slow or skipped sweep without reclassifying the
/// slot as missed.
pub const NATURAL_SLOT_GRACE_SECONDS: i64 = 120;

/// Catalog job the seeded worktree-GC routine dispatches.
///
/// The sweep always catches this job up once, even when an already-seeded
/// definition still says `missed_run: skip`.
pub const WORKTREE_GC_ROUTINE_JOB: &str = "worktree_gc_pipeline";

/// Detail recorded on each slot `missed_run: skip` consumes without firing.
pub const MISSED_RUN_SKIP_REASON: &str = "missed_run: skip";

/// How many skipped slots one pass records. Older slots in a longer gap are
/// named once on the oldest recorded row instead of one row each.
pub const MISSED_SLOT_RECORD_CAP: usize = 48;

/// Derive the natural-slot grace from the cadence of the host clock that
/// invokes the sweep. The scheduler permits one missed or delayed poll, so a
/// slot remains natural for two configured cadence intervals. The default
/// cadence deliberately preserves [`NATURAL_SLOT_GRACE_SECONDS`].
pub fn natural_slot_grace_for_cadence(cadence_seconds: u64) -> Result<Duration, OrbitError> {
    let seconds = cadence_seconds.checked_mul(2).ok_or_else(|| {
        OrbitError::InvalidInput("sweep cadence is too large for natural-slot grace".to_string())
    })?;
    let seconds = i64::try_from(seconds).map_err(|_| {
        OrbitError::InvalidInput("sweep cadence is too large for natural-slot grace".to_string())
    })?;

    Ok(Duration::seconds(seconds))
}

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
    /// `missed_run: skip` and the latest unconsumed slot is past grace.
    ///
    /// `slot` is that latest slot. The sweep records every unconsumed slot
    /// back to the cursor (bounded) and advances the cursor to `slot`, so
    /// the gap is not reconsidered. This is not [`DueDecision::NotDue`]:
    /// nothing is due only when the latest slot is already at or behind the
    /// cursor.
    Skipped {
        /// Latest scheduled slot the skip consumes.
        slot: DateTime<Tz>,
    },
}

/// Slots a skip should record, oldest first, ending at `latest`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissedSlotSpan<Tz: TimeZone> {
    /// Unconsumed slots after `lower_bound`, oldest first, at most `cap`.
    pub slots: Vec<DateTime<Tz>>,
    /// Slots after the cursor and before [`Self::slots`] were not listed.
    pub collapsed_earlier: bool,
}

/// Pin a scheduled occurrence to its minute (seconds and sub-seconds
/// zeroed). Slot identity must be stable across sweeps for the idempotency
/// key to hold.
pub fn truncate_to_minute<Tz: TimeZone>(value: DateTime<Tz>) -> DateTime<Tz> {
    // DateTime's wall-clock setters re-resolve ambiguous local times and can
    // fail during a DST fold. Subtracting the sub-minute duration preserves
    // the instant and its offset, so every scheduled occurrence has a slot.
    let seconds = i64::from(value.second());
    let nanoseconds = i64::from(value.nanosecond());

    value - Duration::seconds(seconds) - Duration::nanoseconds(nanoseconds)
}

/// Parse and validate a routine cron expression (standard 5-field form,
/// evaluated in host-local time by the caller's choice of `Tz`).
pub fn parse_cron(expression: &str) -> Result<Cron, OrbitError> {
    expression.parse::<Cron>().map_err(|error| {
        OrbitError::InvalidInput(format!("invalid cron expression '{expression}': {error}"))
    })
}

/// The next scheduled occurrence strictly after `now`, pinned to its minute.
///
/// This projects the schedule forward; it is not a due decision. A routine
/// carrying a missed slot is still due for that earlier slot under
/// `catch_up_once` ([`due_decision`]) while this reports the upcoming one, so
/// callers rendering both must not expect them to agree.
pub fn next_occurrence<Tz: TimeZone>(
    cron: &Cron,
    now: &DateTime<Tz>,
) -> Result<DateTime<Tz>, OrbitError> {
    let next = cron
        .find_next_occurrence(now, false)
        .map_err(|error| OrbitError::InvalidInput(format!("cron evaluation failed: {error}")))?;

    // croner carries `now`'s sub-minute component into the occurrence it
    // returns, which would move the projection on every poll within the same
    // minute. Pinning is safe here as well as in the due path: the occurrence
    // already lies in a later minute than `now`, so dropping its sub-minute
    // component cannot pull it back to or before `now`.
    Ok(truncate_to_minute(next))
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
    due_decision_with_grace(
        cron,
        missed_run,
        lower_bound,
        now,
        Duration::seconds(NATURAL_SLOT_GRACE_SECONDS),
    )
}

/// Decide whether a routine is due with the natural-slot grace supplied by
/// the host sweep clock. Non-routine callers retain [`due_decision`]'s
/// default cadence behavior.
pub fn due_decision_with_grace<Tz: TimeZone>(
    cron: &Cron,
    missed_run: MissedRunPolicy,
    lower_bound: &DateTime<Tz>,
    now: &DateTime<Tz>,
    natural_slot_grace: Duration,
) -> Result<DueDecision<Tz>, OrbitError> {
    // Latest scheduled slot at or before now.
    let previous = cron
        .find_previous_occurrence(now, true)
        .map_err(|error| OrbitError::InvalidInput(format!("cron evaluation failed: {error}")))?;
    // croner carries `now`'s sub-minute component into the occurrence it
    // returns, which would make the slot different on every sweep within the
    // same minute — breaking the (name, slot) idempotency key. 5-field cron
    // is minute-granular by definition, so pin slots to the minute.
    let previous = truncate_to_minute(previous);

    if previous <= *lower_bound {
        return Ok(DueDecision::NotDue);
    }

    let age = now.clone().signed_duration_since(previous.clone());
    let natural = age <= natural_slot_grace;
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
        // Name the latest missed slot so the sweep can record the gap.
        // The cursor stays put until that recording consumes the slots.
        MissedRunPolicy::Skip => Ok(DueDecision::Skipped { slot: previous }),
    }
}

/// Scheduled slots strictly after `lower_bound` through `latest`, oldest first.
///
/// Walks backward from `latest` and keeps at most `cap` slots, so a long
/// downtime stays bounded. When the gap is longer than `cap`,
/// [`MissedSlotSpan::collapsed_earlier`] is set and the retained slots are
/// the newest ones, still ending at `latest`.
pub fn missed_slot_span<Tz: TimeZone>(
    cron: &Cron,
    lower_bound: &DateTime<Tz>,
    latest: &DateTime<Tz>,
    cap: usize,
) -> Result<MissedSlotSpan<Tz>, OrbitError> {
    if cap == 0 || *latest <= *lower_bound {
        return Ok(MissedSlotSpan {
            slots: Vec::new(),
            collapsed_earlier: false,
        });
    }

    let mut newest_first = Vec::new();
    let mut cursor = latest.clone();
    let mut collapsed_earlier = false;
    loop {
        if cursor <= *lower_bound {
            break;
        }
        if newest_first.len() == cap {
            collapsed_earlier = true;
            break;
        }
        newest_first.push(cursor.clone());
        let previous = match cron.find_previous_occurrence(&cursor, false) {
            Ok(previous) => truncate_to_minute(previous),
            Err(_) => break,
        };
        if previous >= cursor {
            break;
        }
        cursor = previous;
    }
    newest_first.reverse();
    Ok(MissedSlotSpan {
        slots: newest_first,
        collapsed_earlier,
    })
}

/// Length of the cron period that contains `now`: the previous occurrence
/// through the next one. DST folds make this 23 or 25 hours for a daily
/// schedule; callers use the actual gap rather than a nominal cadence.
pub fn schedule_interval<Tz: TimeZone>(
    cron: &Cron,
    now: &DateTime<Tz>,
) -> Result<Duration, OrbitError> {
    let previous = cron
        .find_previous_occurrence(now, true)
        .map_err(|error| OrbitError::InvalidInput(format!("cron evaluation failed: {error}")))?;
    let previous = truncate_to_minute(previous);
    let next = cron
        .find_next_occurrence(&previous, false)
        .map_err(|error| OrbitError::InvalidInput(format!("cron evaluation failed: {error}")))?;
    let next = truncate_to_minute(next);
    let interval = next.signed_duration_since(previous);
    if interval <= Duration::zero() {
        return Err(OrbitError::InvalidInput(
            "cron interval did not advance".to_string(),
        ));
    }
    Ok(interval)
}
