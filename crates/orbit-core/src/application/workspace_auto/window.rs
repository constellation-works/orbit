use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};
use orbit_engine::DispatchError;
use orbit_types::workflow::PipelineState;
use serde::Deserialize;
use serde::de::Deserializer;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::workflow::MAX_DRAIN_WINDOW_SECONDS;

/// Open or re-read a drain window [ORB-10819].
///
/// Two call shapes, one action. Called with `for_seconds` and no `deadline` it
/// *stamps*: the deadline is `now + for_seconds`, returned as RFC3339. Called
/// with that `deadline` echoed back it *answers*: whether the window has since
/// expired. The stamp therefore lives in the stamping step's own pipeline
/// output, which the run state already persists — no new durable artifact, and
/// re-reading the window is a pure function of a value the run carries.
///
/// An answer reads the drain's own run state too: an operator stop closes the
/// window early, and an operator extension (`orbit run extend`, ORB-15383)
/// replaces the echoed deadline when it is later. The answer reports the
/// deadline in force, so the run's own step outputs show the extension.
///
/// A zero or absent window is expired on the first answer. `break_when` is
/// evaluated after a loop body runs, so that yields exactly one iteration:
/// the one-tick behavior every pre-window caller of `orbit run auto` has.
///
/// The deadline gates *starting* work. Nothing here cancels anything, which is
/// what makes "the window does not affect tasks already in progress" true by
/// construction: an in-flight child is held by `invoke_and_wait`, not by the
/// window.
pub(crate) fn drain_window(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    let now = Utc::now();
    let request = decode_drain_window_request(action, input)?;
    let state = read_drain_state(runtime, request.run_id());
    let deadline = match request {
        DrainWindowRequest::Echoed { deadline, .. } => state
            .as_ref()
            .map_or(deadline, |state| state.effective_drain_deadline(deadline)),
        DrainWindowRequest::Open { seconds, .. } => now
            .checked_add_signed(seconds_to_delta(action, seconds)?)
            .ok_or_else(|| {
                DispatchError::action_failed(
                    action,
                    format!("`for_seconds` {seconds} overflows the drain deadline"),
                )
            })?,
    };

    let remaining_seconds = (deadline - now).num_milliseconds() as f64 / 1000.0;
    let window_expired = remaining_seconds <= 0.0;
    let closed = state.is_some_and(|state| state.admissions_stopped());
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

/// Decoded drain-window input. Callers past `decode_drain_window_request`
/// see only this value: an echoed stamp, or a duration still to be stamped.
///
/// A parsed deadline never consults `for_seconds`, so an invalid duration
/// beside a valid echo stays ignored.
enum DrainWindowRequest {
    Echoed {
        deadline: DateTime<Utc>,
        run_id: Option<String>,
    },
    Open {
        seconds: f64,
        run_id: Option<String>,
    },
}

impl DrainWindowRequest {
    fn run_id(&self) -> Option<&str> {
        match self {
            Self::Echoed { run_id, .. } | Self::Open { run_id, .. } => run_id.as_deref(),
        }
    }
}

/// The activity's `deadline`, `for_seconds` and `run_id`, read once.
///
/// A non-object has no fields. That is the same silent zero window `.get`
/// produced, so serde's type error falls back to the defaults instead of
/// becoming a new failure.
fn decode_drain_window_request(
    action: &str,
    input: &Value,
) -> Result<DrainWindowRequest, DispatchError> {
    let fields = match DrainWindowFields::deserialize(input) {
        Ok(fields) => fields,
        Err(_) if !input.is_object() => DrainWindowFields::default(),
        Err(error) => {
            return Err(DispatchError::action_failed(
                action,
                format!("drain window input could not be read: {error}"),
            ));
        }
    };
    fields.into_request(action)
}

/// Wire fields before deadline-versus-duration precedence. Unknown keys,
/// including the engine's `step_id` and `job_run_id`, are ignored.
#[derive(Default, Deserialize)]
struct DrainWindowFields {
    #[serde(default)]
    deadline: CoercedDrainDeadline,
    #[serde(default)]
    for_seconds: CoercedWindowSeconds,
    #[serde(default)]
    run_id: CoercedDrainRunId,
}

impl DrainWindowFields {
    fn into_request(self, action: &str) -> Result<DrainWindowRequest, DispatchError> {
        let run_id = self.run_id.into_inner();
        match self.deadline {
            CoercedDrainDeadline::Malformed { raw, error } => Err(DispatchError::action_failed(
                action,
                format!("`deadline` '{raw}' is not RFC3339: {error}"),
            )),
            // Valid echo wins before `for_seconds` is judged.
            CoercedDrainDeadline::Parsed(deadline) => {
                Ok(DrainWindowRequest::Echoed { deadline, run_id })
            }
            CoercedDrainDeadline::Absent => Ok(DrainWindowRequest::Open {
                seconds: self.for_seconds.validated(action)?,
                run_id,
            }),
        }
    }
}

/// `deadline` counts only as a nonblank string. Anything else is absent and
/// the duration path runs. A nonblank string that is not RFC3339 is an error.
#[derive(Default)]
enum CoercedDrainDeadline {
    #[default]
    Absent,
    Parsed(DateTime<Utc>),
    Malformed {
        raw: String,
        error: chrono::format::ParseError,
    },
}

impl<'de> Deserialize<'de> for CoercedDrainDeadline {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let Some(raw) = value
            .as_str()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        else {
            return Ok(Self::Absent);
        };
        Ok(match DateTime::parse_from_rfc3339(raw) {
            Ok(parsed) => Self::Parsed(parsed.with_timezone(&Utc)),
            Err(error) => Self::Malformed {
                raw: raw.to_string(),
                error,
            },
        })
    }
}

/// `for_seconds` before the range check. Missing, null and blank stay zero
/// so a valid echoed deadline can ignore them without failing.
#[derive(Default)]
enum CoercedWindowSeconds {
    #[default]
    Zero,
    Seconds(f64),
    /// JSON number that `Number::as_f64` refuses.
    NotAnF64,
    NotANumber {
        text: String,
        error: std::num::ParseFloatError,
    },
    WrongType {
        rendered: String,
    },
}

impl CoercedWindowSeconds {
    fn validated(self, action: &str) -> Result<f64, DispatchError> {
        let seconds = match self {
            Self::Zero => 0.0,
            Self::Seconds(seconds) => seconds,
            Self::NotAnF64 => {
                return Err(DispatchError::action_failed(
                    action,
                    "`for_seconds` is not a finite number",
                ));
            }
            Self::NotANumber { text, error } => {
                return Err(DispatchError::action_failed(
                    action,
                    format!("`for_seconds` '{text}' is not a number: {error}"),
                ));
            }
            Self::WrongType { rendered } => {
                return Err(DispatchError::action_failed(
                    action,
                    format!("`for_seconds` must be a number, got {rendered}"),
                ));
            }
        };
        if !seconds.is_finite() || !(0.0..=MAX_DRAIN_WINDOW_SECONDS as f64).contains(&seconds) {
            return Err(DispatchError::action_failed(
                action,
                format!("`for_seconds` must be between 0 and {MAX_DRAIN_WINDOW_SECONDS}"),
            ));
        }
        Ok(seconds)
    }
}

impl<'de> Deserialize<'de> for CoercedWindowSeconds {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Ok(match value {
            Value::Null => Self::Zero,
            Value::Number(number) => match number.as_f64() {
                Some(seconds) => Self::Seconds(seconds),
                None => Self::NotAnF64,
            },
            Value::String(text) => {
                let text = text.trim();
                if text.is_empty() {
                    Self::Zero
                } else {
                    match text.parse::<f64>() {
                        Ok(seconds) => Self::Seconds(seconds),
                        Err(error) => Self::NotANumber {
                            text: text.to_string(),
                            error,
                        },
                    }
                }
            }
            other => Self::WrongType {
                rendered: other.to_string(),
            },
        })
    }
}

/// Engine-injected drain run id: a trimmed nonblank string, or no state.
#[derive(Default)]
struct CoercedDrainRunId(Option<String>);

impl CoercedDrainRunId {
    fn into_inner(self) -> Option<String> {
        self.0
    }
}

impl<'de> Deserialize<'de> for CoercedDrainRunId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Ok(Self(
            value
                .as_str()
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_string),
        ))
    }
}

fn read_drain_state(runtime: &OrbitRuntime, run_id: Option<&str>) -> Option<PipelineState> {
    let run_id = run_id?;
    runtime.read_run_state(run_id).ok().flatten()
}

fn seconds_to_delta(action: &str, seconds: f64) -> Result<TimeDelta, DispatchError> {
    TimeDelta::try_milliseconds((seconds * 1000.0).round() as i64).ok_or_else(|| {
        DispatchError::action_failed(action, format!("`for_seconds` {seconds} is out of range"))
    })
}
