//! Scheduled tasks: a `not_before` instant before which no dispatcher starts
//! a `backlog` task.
//!
//! The hold is an admission reason, not a lifecycle state: it lifts by itself
//! on the first admission pass after the time, with no timer. An operator may
//! start the task early from the CLI (`orbit run ship --ignore-schedule`),
//! which records a [`SCHEDULE_OVERRIDDEN_EVENT`] naming the scheduled time
//! before the run is submitted. Every admission route reads that event, so
//! the override covers the run from submission through dispatch and resume.
//! Changing `not_before` afterwards reinstates the hold for the new time.

use chrono::{DateTime, SecondsFormat, Timelike, Utc};

use crate::task::{Task, TaskError, TaskHistoryEntry};

/// The task field that holds the scheduled time.
pub const NOT_BEFORE_FIELD: &str = "not_before";

/// The history event an operator's early start of a scheduled task records.
pub const SCHEDULE_OVERRIDDEN_EVENT: &str = "schedule_overridden";

/// How every scheduled-hold detail begins, so a surface that receives only
/// the sentence (a pull follower reading the owner's hold) can classify it.
pub const SCHEDULED_HOLD_PREFIX: &str = "scheduled: not before ";

/// The canonical spelling of a scheduled time: RFC 3339 in UTC, to the second.
#[must_use]
pub fn format_not_before(not_before: DateTime<Utc>) -> String {
    not_before.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Parse an RFC 3339 instant with an explicit offset into UTC, to the second,
/// refusing anything else and naming the input.
pub fn parse_not_before(raw: &str) -> Result<DateTime<Utc>, TaskError> {
    let trimmed = raw.trim();
    DateTime::parse_from_rfc3339(trimmed)
        .map(|instant| {
            let utc = instant.with_timezone(&Utc);
            utc.with_nanosecond(0).unwrap_or(utc)
        })
        .map_err(|error| {
            TaskError::Invalid(format!(
                "`{NOT_BEFORE_FIELD}` must be an RFC 3339 instant with an offset, such as \
                 `2026-11-06T09:00:00Z`; got `{trimmed}` ({error})"
            ))
        })
}

/// Why a scheduled task waits, naming the time.
#[must_use]
pub fn scheduled_wait(not_before: DateTime<Utc>) -> String {
    format!(
        "{SCHEDULED_HOLD_PREFIX}{}; the hold lifts on the first admission pass after that time",
        format_not_before(not_before)
    )
}

/// The note of a [`SCHEDULE_OVERRIDDEN_EVENT`]. It leads with the scheduled
/// time, which is what [`schedule_overridden`] matches.
#[must_use]
pub fn schedule_override_note(not_before: DateTime<Utc>, by: &str) -> String {
    format!(
        "{}; operator `{by}` started it early with `orbit run ship --ignore-schedule`",
        override_note_lead(not_before)
    )
}

fn override_note_lead(not_before: DateTime<Utc>) -> String {
    format!("scheduled for {}", format_not_before(not_before))
}

/// Whether `history` records an operator override for `not_before`.
#[must_use]
pub fn schedule_overridden(history: &[TaskHistoryEntry], not_before: DateTime<Utc>) -> bool {
    let lead = override_note_lead(not_before);
    history.iter().any(|entry| {
        entry.event == SCHEDULE_OVERRIDDEN_EVENT
            && entry
                .note
                .as_deref()
                .is_some_and(|note| note.starts_with(&lead))
    })
}

impl Task {
    /// The task's `not_before` while it is still in the future at `now`.
    /// Status is not consulted: callers apply the hold to `backlog` only.
    #[must_use]
    pub fn scheduled_after(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.not_before.filter(|not_before| *not_before > now)
    }
}
