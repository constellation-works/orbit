use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::{
    RESIDENT_AUTHORITY_CHANGED_EVENT, RESIDENT_CYCLE_GRANTED_EVENT, ResidentComment,
    ResidentLiveCycle, ResidentMandate,
};

use super::super::control::{
    RESIDENT_CONTROL_EVENT, ResidentControlAction, ResidentControlEntry, ResidentControlState,
};
use super::super::cycle::CYCLE_CHANGED;
use super::super::resident_mandate_ended;
use crate::application::statistics;

/// The reversal window of design §9: a human undo within 7 days of the
/// resident decision or action it undoes.
pub(super) const REVERSAL_WINDOW_DAYS: i64 = 7;
/// Default metrics window when `--since` is absent.
const DEFAULT_WINDOW_DAYS: i64 = 30;

pub(super) fn is_human_writer(by: &str) -> bool {
    let by = by.trim();
    by == "human" || by == "operator" || by.starts_with("human:") || by.starts_with("human-")
}

/// Custom process actor labels carry no human prefix. Count one only when the
/// control record explicitly captured a human actor, while preserving the
/// legacy label check and excluding system and agent writers.
pub(super) fn is_explicit_human_control_writer(entry: &TaskHistoryEntry) -> bool {
    if entry.event != RESIDENT_CONTROL_EVENT {
        return false;
    }
    let Some(note) = entry.note.as_deref() else {
        return false;
    };
    let Ok(control) = serde_json::from_str::<ResidentControlEntry>(note) else {
        return false;
    };
    let writer = entry.by.trim();
    control.human_actor
        && control.by == entry.by
        && writer != "unknown"
        && !orbit_types::identity::is_automation_actor(writer)
}

/// The action of a `resident_control` entry that counts as a manual
/// intervention (design §9): a `stop` or a `takeover`. Wake, pause, resume
/// and release do not count.
pub(super) fn counted_control_action(entry: &TaskHistoryEntry) -> Option<ResidentControlAction> {
    let note: ResidentControlEntry = serde_json::from_str(entry.note.as_deref()?).ok()?;
    matches!(
        note.action,
        ResidentControlAction::Stop | ResidentControlAction::Takeover
    )
    .then_some(note.action)
}

pub(in super::super) fn parse_window_start(
    since: Option<&str>,
    now: DateTime<Utc>,
) -> Result<DateTime<Utc>, OrbitError> {
    let Some(raw) = since.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(now - Duration::days(DEFAULT_WINDOW_DAYS));
    };
    if let Ok(at) = DateTime::parse_from_rfc3339(raw) {
        return Ok(at.with_timezone(&Utc));
    }
    if let Ok(day) = chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        && let Some(at) = day.and_hms_opt(0, 0, 0)
    {
        return Ok(DateTime::from_naive_utc_and_offset(at, Utc));
    }
    let compact: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
    let compact = compact.trim_end_matches('s');
    let split = compact
        .find(|c: char| c.is_ascii_alphabetic())
        .ok_or_else(|| OrbitError::InvalidInput(format!("unrecognised `--since` window: {raw}")))?;
    let (amount, unit) = compact.split_at(split);
    let amount: i64 = amount
        .parse()
        .map_err(|_| OrbitError::InvalidInput(format!("unrecognised `--since` window: {raw}")))?;
    if amount < 0 {
        return Err(OrbitError::InvalidInput(format!(
            "unrecognised `--since` window: {raw}"
        )));
    }
    let span = match unit {
        "m" | "min" | "minute" => Duration::try_minutes(amount),
        "h" | "hour" => Duration::try_hours(amount),
        "d" | "day" => Duration::try_days(amount),
        "w" | "week" => Duration::try_weeks(amount),
        _ => {
            return Err(OrbitError::InvalidInput(format!(
                "unrecognised `--since` window: {raw} (use e.g. 7d, 24h, 2w or an RFC 3339 instant)"
            )));
        }
    };
    span.and_then(|span| now.checked_sub_signed(span))
        .ok_or_else(|| OrbitError::InvalidInput(format!("out of range `--since` window: {raw}")))
}

/// The median of `values` without reordering them: callers also report the
/// samples in the order they were recorded.
pub(super) fn median_seconds(values: &[f64]) -> Option<f64> {
    statistics::median(&mut values.to_vec())
}

pub(super) fn active_days(span_start: DateTime<Utc>, span_end: DateTime<Utc>) -> f64 {
    let seconds = (span_end - span_start).num_seconds().max(0) as f64;
    (seconds / 86_400.0).ceil().max(1.0)
}

pub(super) fn assignment_closed_at(
    task: &Task,
    history: &[TaskHistoryEntry],
) -> Option<DateTime<Utc>> {
    let is_closed =
        |status: TaskStatus| resident_mandate_ended(status) || status == TaskStatus::Review;
    if !is_closed(task.status) {
        return None;
    }
    let mut closed_at = None;
    for entry in history.iter().rev() {
        if let Some(to_status) = entry.to_status {
            if is_closed(to_status) {
                closed_at = Some(entry.at);
            } else {
                break;
            }
        }
    }
    Some(closed_at.unwrap_or(task.updated_at))
}

pub(super) fn mandate_activated_at(
    task: &Task,
    history: &[TaskHistoryEntry],
    comments: &[orbit_types::task::TaskComment],
    manifest_created_at: Option<DateTime<Utc>>,
) -> DateTime<Utc> {
    let first_cycle_at = history
        .iter()
        .filter(|entry| {
            entry.event == RESIDENT_CYCLE_GRANTED_EVENT
                || (entry.event == CYCLE_CHANGED
                    && entry
                        .note
                        .as_deref()
                        .and_then(|note| {
                            serde_json::from_str::<Option<ResidentLiveCycle>>(note).ok()
                        })
                        .flatten()
                        .is_some())
        })
        .map(|entry| entry.at)
        .min();
    let first_checkpoint_at = comments
        .iter()
        .filter(|comment| {
            matches!(
                ResidentComment::parse(&comment.message),
                Ok(ResidentComment::Checkpoint(record)) if record.mandate == task.id.as_str()
            )
        })
        .map(|comment| comment.at)
        .min();
    let first_authority_at = history
        .iter()
        .filter(|entry| entry.event == RESIDENT_AUTHORITY_CHANGED_EVENT)
        .map(|entry| entry.at)
        .min();
    let first_control_at = history
        .iter()
        .filter(|entry| entry.event == RESIDENT_CONTROL_EVENT)
        .map(|entry| entry.at)
        .min();
    let first_system_activation_at = history
        .iter()
        .filter(|entry| {
            entry.by == "system"
                && entry.to_status == Some(TaskStatus::InProgress)
                && entry.event != "started"
                && entry.decision.is_none()
        })
        .map(|entry| entry.at)
        .min();
    [
        manifest_created_at,
        first_cycle_at,
        first_checkpoint_at,
        first_authority_at,
        first_control_at,
        first_system_activation_at,
    ]
    .into_iter()
    .flatten()
    .min()
    .unwrap_or(task.created_at)
}

pub(super) fn mandate_ended_at(
    task: &Task,
    history: &[TaskHistoryEntry],
    mandate: Option<&ResidentMandate>,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    let stop_at = history
        .iter()
        .filter(|entry| entry.event == RESIDENT_CONTROL_EVENT)
        .filter_map(|entry| {
            let parsed =
                serde_json::from_str::<ResidentControlEntry>(entry.note.as_deref()?).ok()?;
            (parsed.state == ResidentControlState::Ended
                || parsed.action == ResidentControlAction::Stop)
                .then_some(entry.at)
        })
        .min();
    let deadline_at = mandate
        .and_then(|record| record.deadline)
        .filter(|deadline| *deadline <= now);
    let closed_at = assignment_closed_at(task, history);
    [stop_at, deadline_at, closed_at]
        .into_iter()
        .flatten()
        .min()
}
