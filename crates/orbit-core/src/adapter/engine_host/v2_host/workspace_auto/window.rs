use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};
use orbit_engine::DispatchError;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::workflow::MAX_DRAIN_WINDOW_SECONDS;

use super::action_failed;
use super::drains::live_admissions_stop;

/// Open or re-read a drain window [ORB-10819].
///
/// Two call shapes, one action. Called with `for_seconds` and no `deadline` it
/// *stamps*: the deadline is `now + for_seconds`, returned as RFC3339. Called
/// with that `deadline` echoed back it *answers*: whether the window has since
/// expired. The stamp therefore lives in the stamping step's own pipeline
/// output, which the run state already persists — no new durable artifact, and
/// re-reading the window is a pure function of a value the run carries.
///
/// A zero or absent window is expired on the first answer. `break_when` is
/// evaluated after a loop body runs, so that yields exactly one iteration:
/// the one-tick behavior every pre-window caller of `orbit run auto` has.
///
/// The deadline gates *starting* work. Nothing here cancels anything, which is
/// what makes "the window does not affect tasks already in progress" true by
/// construction: an in-flight child is held by `invoke_and_wait`, not by the
/// window.
pub(in super::super) fn drain_window(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    let now = Utc::now();
    let deadline = match optional_deadline(action, input)? {
        Some(deadline) => deadline,
        None => {
            let for_seconds = window_seconds(action, input)?;
            now.checked_add_signed(seconds_to_delta(action, for_seconds)?)
                .ok_or_else(|| {
                    action_failed(
                        action,
                        format!("`for_seconds` {for_seconds} overflows the drain deadline"),
                    )
                })?
        }
    };

    let remaining_seconds = (deadline - now).num_milliseconds() as f64 / 1000.0;
    let window_expired = remaining_seconds <= 0.0;
    let closed = live_admissions_stop(runtime, input).is_some();
    let expired = window_expired || closed;
    Ok(json!({
        "deadline": deadline.to_rfc3339_opts(SecondsFormat::Secs, true),
        "expired": expired,
        "remaining_seconds": if closed { 0.0 } else { remaining_seconds.max(0.0) },
        "expired_reason": if closed {
            "admissions_stopped"
        } else if window_expired {
            "window"
        } else {
            "open"
        },
    }))
}

fn optional_deadline(action: &str, input: &Value) -> Result<Option<DateTime<Utc>>, DispatchError> {
    let Some(raw) = input
        .get("deadline")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    DateTime::parse_from_rfc3339(raw)
        .map(|parsed| Some(parsed.with_timezone(&Utc)))
        .map_err(|err| action_failed(action, format!("`deadline` '{raw}' is not RFC3339: {err}")))
}

/// Read `for_seconds`, tolerating the string a template renders when the
/// caller supplied no window at all (`"{{ input.for_seconds }}"` over an
/// absent key resolves to an empty string, not to JSON `null`).
fn window_seconds(action: &str, input: &Value) -> Result<f64, DispatchError> {
    let Some(raw) = input.get("for_seconds") else {
        return Ok(0.0);
    };
    let seconds = match raw {
        Value::Null => 0.0,
        Value::Number(number) => number
            .as_f64()
            .ok_or_else(|| action_failed(action, "`for_seconds` is not a finite number".into()))?,
        Value::String(text) => {
            let text = text.trim();
            if text.is_empty() {
                0.0
            } else {
                text.parse::<f64>().map_err(|err| {
                    action_failed(
                        action,
                        format!("`for_seconds` '{text}' is not a number: {err}"),
                    )
                })?
            }
        }
        other => {
            return Err(action_failed(
                action,
                format!("`for_seconds` must be a number, got {other}"),
            ));
        }
    };
    if !seconds.is_finite() || !(0.0..=MAX_DRAIN_WINDOW_SECONDS as f64).contains(&seconds) {
        return Err(action_failed(
            action,
            format!("`for_seconds` must be between 0 and {MAX_DRAIN_WINDOW_SECONDS}"),
        ));
    }
    Ok(seconds)
}

fn seconds_to_delta(action: &str, seconds: f64) -> Result<TimeDelta, DispatchError> {
    TimeDelta::try_milliseconds((seconds * 1000.0).round() as i64)
        .ok_or_else(|| action_failed(action, format!("`for_seconds` {seconds} is out of range")))
}
