//! Blocking wait on an attention delta for a shell caller.
//!
//! The wait polls the same derivation the MCP read uses, through a session
//! minted grant ([`super::wait_grant`]), and returns on the first of: a
//! non-empty delta, a changed `degraded` set, or the deadline. It spends no
//! model turn while nothing changes. A source it cannot read is never
//! reported as clean: the timeout answer carries the current `degraded`
//! entries, and a read that failed to the deadline is its own outcome.
use orbit_common::{OrbitError, protocol::tool_input::parse_duration_seconds};
use serde_json::Value;
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

/// Default and minimum gap between polls, in seconds.
const DEFAULT_POLL_INTERVAL_SECONDS: u64 = 15;
const MIN_POLL_INTERVAL_SECONDS: u64 = 5;
/// Longest a single wait may run, in seconds.
const MAX_WAIT_SECONDS: u64 = 24 * 3600;

/// Why an attention wait returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttentionWaitStatus {
    /// The delta since the cursor is non-empty.
    Changed,
    /// The delta is empty but the `degraded` set differs from the first read.
    DegradedChanged,
    /// The deadline passed with an empty delta and an unchanged `degraded` set.
    /// The answer still lists whatever was degraded.
    TimedOut,
    /// Every read up to the deadline failed, so nothing is known.
    Unreadable,
}

/// The result of one wait.
#[derive(Debug, Clone)]
pub struct AttentionWaitOutcome {
    /// Why the wait returned.
    pub status: AttentionWaitStatus,
    /// The newest attention envelope, absent for [`AttentionWaitStatus::Unreadable`].
    pub response: Option<Value>,
    /// The last read error, set for [`AttentionWaitStatus::Unreadable`].
    pub error: Option<String>,
    /// Whole seconds the wait ran.
    pub waited_seconds: u64,
}

/// The identity of a degraded entry, ignoring its free-text message.
fn degraded_signature(response: &Value) -> BTreeSet<String> {
    response["degraded"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|entry| {
            format!(
                "{}|{}|{}",
                entry["scope"].as_str().unwrap_or_default(),
                entry["kind"].as_str().unwrap_or_default(),
                entry["code"].as_str().unwrap_or_default()
            )
        })
        .collect()
}

/// Wait for the attention delta since `input.since` to become non-empty or
/// for its `degraded` set to change, for at most `timeout`.
///
/// `read` performs one attention read for an input. `input` is an
/// `orbit.attention` input with a required `since` cursor and the ordinary
/// filters. A refused read or invalid input is an error at once; only
/// transient read failures are retried until the deadline.
pub(crate) fn wait_for_attention_delta(
    mut input: Value,
    timeout: &str,
    interval: Option<&str>,
    read: &mut dyn FnMut(Value) -> Result<Value, OrbitError>,
) -> Result<AttentionWaitOutcome, OrbitError> {
    if input["since"]
        .as_str()
        .is_none_or(|since| since.trim().is_empty())
    {
        return Err(OrbitError::InvalidInput(
            "attention wait needs --since <cursor.next from a previous read>".into(),
        ));
    }
    let timeout_seconds = parse_duration_seconds(timeout)?.clamp(1, MAX_WAIT_SECONDS);
    let interval_seconds = match interval {
        Some(raw) => parse_duration_seconds(raw)?.max(MIN_POLL_INTERVAL_SECONDS),
        None => DEFAULT_POLL_INTERVAL_SECONDS,
    };
    let started = Instant::now();
    let deadline = started + Duration::from_secs(timeout_seconds);
    let mut baseline: Option<BTreeSet<String>> = None;
    let mut last: Option<Value> = None;
    let mut last_error: Option<String>;
    loop {
        let waited_seconds = started.elapsed().as_secs();
        match read(input.clone()) {
            Ok(response) => {
                last_error = None;
                let has_items = response["items"].as_array().is_some_and(|i| !i.is_empty());
                let signature = degraded_signature(&response);
                let status = if has_items {
                    Some(AttentionWaitStatus::Changed)
                } else if baseline.as_ref().is_some_and(|seen| *seen != signature) {
                    Some(AttentionWaitStatus::DegradedChanged)
                } else {
                    None
                };
                baseline.get_or_insert(signature);
                if let Some(next) = response["cursor"]["next"].as_str() {
                    input["since"] = Value::String(next.to_owned());
                }
                if let Some(status) = status {
                    return Ok(AttentionWaitOutcome {
                        status,
                        response: Some(response),
                        error: None,
                        waited_seconds,
                    });
                }
                last = Some(response);
            }
            Err(error @ (OrbitError::CapabilityDenied(_) | OrbitError::InvalidInput(_))) => {
                return Err(error);
            }
            Err(error) => last_error = Some(error.to_string()),
        }
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_secs(interval_seconds).min(deadline - now));
    }
    let waited_seconds = started.elapsed().as_secs();
    // The newest read decides: a failing last read must not hide behind an
    // earlier good one.
    Ok(match (last, last_error) {
        (Some(response), None) => AttentionWaitOutcome {
            status: AttentionWaitStatus::TimedOut,
            response: Some(response),
            error: None,
            waited_seconds,
        },
        (_, error) => AttentionWaitOutcome {
            status: AttentionWaitStatus::Unreadable,
            response: None,
            error: error.or_else(|| Some("attention could not be read".into())),
            waited_seconds,
        },
    })
}
