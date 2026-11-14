//! The drain job's templated activity input, which renders scalars as strings.

use chrono::Duration;
use orbit_engine::DispatchError;
use orbit_store::contracts::PullDestination;
use serde_json::Value;

use super::super::drain::DEFAULT_BREAKER_COOLDOWN_SECONDS;

const DEFAULT_MAX_ACTIVE_LEAF_RUNS: u64 = 5;
const DEFAULT_POLL_SLEEP_SECONDS: u64 = 30;
const DEFAULT_IDLE_SLEEP_SECONDS: u64 = 60;

/// Scalars one `pull_refill` pass reads from its activity input.
pub(super) struct RefillInput {
    pub(super) run_id: String,
    pub(super) destination: PullDestination,
    /// The window the job forwarded as already closed.
    pub(super) forwarded_expired: bool,
    pub(super) single_pass: bool,
    pub(super) ceiling: u64,
    pub(super) poll: u64,
    pub(super) idle: u64,
    /// Cooldown the failure breaker was submitted with, as a duration.
    pub(super) cooldown: Duration,
}

impl RefillInput {
    /// Parse the pass input. A missing run id or destination fails the action;
    /// the other scalars fall back to the drain's defaults.
    pub(super) fn parse(action: &str, input: &Value) -> Result<Self, DispatchError> {
        let failed = |message: String| DispatchError::DeterministicActionFailed {
            action: action.to_string(),
            message,
        };
        let run_id = input
            .get("run_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| failed("the executing drain run id was not supplied".into()))?
            .to_string();
        let destination = input
            .get("destination")
            .cloned()
            .ok_or_else(|| failed("`destination` is required".into()))
            .and_then(|value| {
                serde_json::from_value(value)
                    .map_err(|error| failed(format!("`destination`: {error}")))
            })?;
        let cooldown = u64_input(
            input,
            "breaker_cooldown_seconds",
            DEFAULT_BREAKER_COOLDOWN_SECONDS,
        );
        let cooldown = i64::try_from(cooldown)
            .ok()
            .and_then(Duration::try_seconds)
            .unwrap_or(Duration::MAX);
        Ok(Self {
            run_id,
            destination,
            forwarded_expired: bool_input(input, "window_expired"),
            single_pass: single_pass_input(input),
            ceiling: u64_input(input, "max_active_leaf_runs", DEFAULT_MAX_ACTIVE_LEAF_RUNS),
            poll: u64_input(input, "poll_sleep_seconds", DEFAULT_POLL_SLEEP_SECONDS),
            idle: u64_input(input, "idle_sleep_seconds", DEFAULT_IDLE_SLEEP_SECONDS),
            cooldown,
        })
    }
}

/// Whether the drain was submitted without a window: `for_seconds` zero, as
/// the job forwards it. A pass given no `for_seconds` at all is a timed one.
pub(super) fn single_pass_input(input: &Value) -> bool {
    match input.get("for_seconds") {
        Some(Value::Number(number)) => number.as_f64() == Some(0.0),
        Some(Value::String(text)) => {
            let text = text.trim();
            text.is_empty() || text.parse::<f64>().is_ok_and(|seconds| seconds == 0.0)
        }
        Some(Value::Null) => true,
        _ => false,
    }
}

/// A boolean templated into activity input, which renders as a string.
pub(super) fn bool_input(input: &Value, key: &str) -> bool {
    match input.get(key) {
        Some(Value::Bool(value)) => *value,
        Some(Value::String(text)) => text.trim() == "true",
        _ => false,
    }
}

pub(super) fn u64_input(input: &Value, key: &str, default: u64) -> u64 {
    match input.get(key) {
        Some(Value::Number(number)) => number.as_u64().unwrap_or(default),
        Some(Value::String(text)) => text.trim().parse().unwrap_or(default),
        _ => default,
    }
}
