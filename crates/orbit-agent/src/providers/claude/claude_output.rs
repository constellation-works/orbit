//! Claude Code's `--output-format stream-json --verbose` stdout. [ORB-14696]
//!
//! The stream carries every message of the session: `system`, `assistant`
//! and `user` messages, `rate_limit_event`s, and one terminal `result` per
//! turn. Orbit reads two things from it:
//!
//! - The answer is the last `result` frame alone, the same object
//!   `--output-format json` prints. Assistant and tool messages carry their own
//!   per-message `usage`, and the `StructuredOutput` tool call quotes the
//!   envelope, so neither may reach the response or usage projection
//!   (ORB-10906 / F2026-08-031).
//! - The usage windows are the last `rate_limit_event`'s `rate_limit_info`,
//!   which the CLI emits when a window's rounded percentage or reset moves.
//! - Assistant messages' own `usage` shows whether any request's prompt may
//!   reach a prompt-size price tier; it carries no request's final output, so
//!   the totals stay the `result` frame's alone.

use chrono::{DateTime, Utc};
use orbit_types::telemetry::{ProviderLimitObservation, ProviderLimitSource, TokenUsage};
use serde_json::{Map, Value};

use super::super::usage_window::{percent, top_level_frames};
use crate::types::request_usage;

/// Return the terminal `result` frame when `stdout` is a Claude message
/// stream.
///
/// `None` means the capture holds no stream messages, as the single document
/// of `--output-format json` does, and leaves it unchanged. A stream with no
/// `result` (a run cut off mid-turn) projects to nothing, so no envelope
/// quoted by an earlier message can stand in for the answer.
pub(crate) fn project_claude_response(stdout: &[u8]) -> Option<Vec<u8>> {
    let mut saw_stream_message = false;
    let mut terminal_result = None;
    for (line, frame) in top_level_frames(stdout) {
        match frame.get("type").and_then(Value::as_str) {
            Some("result") => terminal_result = Some(line),
            Some(_) => saw_stream_message = true,
            None => {}
        }
    }
    saw_stream_message.then(|| terminal_result.unwrap_or_default().to_vec())
}

/// The usage of each API request on a Claude message stream whose prompt may
/// reach a prompt-size price tier: empty when the stream shows none can, and
/// `None` when that is unknown.
///
/// The CLI writes one `assistant` message per content block, each repeating
/// its request's `usage` as of the request's start, so a message's
/// `output_tokens` is not the request's final output (ORB-19680 measured 232
/// streamed against 73,963 billed). The input-side buckets are final, so a
/// stream whose requests all stay below the capture floor proves no request
/// reached a tier; one that may reach it leaves the run's per-request usage
/// unknown rather than underpriced. A stream without its `result` (cut off)
/// or the single document of `--output-format json` is `None` too. A request
/// on another model than `model` (a subagent's, say) is not priced at this
/// invocation's tier and is left out; a context-window suffix such as `[1m]`
/// does not make a model another one.
pub(crate) fn claude_long_prompt_requests(
    stdout: &[u8],
    model: Option<&str>,
) -> Option<Vec<TokenUsage>> {
    let mut saw_result = false;
    let mut saw_stream_message = false;
    for (_, frame) in top_level_frames(stdout) {
        match frame.get("type").and_then(Value::as_str) {
            Some("result") => saw_result = true,
            Some("assistant") => {
                saw_stream_message = true;
                let Some(message) = frame.get("message").and_then(Value::as_object) else {
                    continue;
                };
                let request_model = message.get("model").and_then(Value::as_str);
                if model
                    .zip(request_model)
                    .is_some_and(|(model, request)| base_model(model) != base_model(request))
                {
                    continue;
                }
                if message
                    .get("usage")
                    .and_then(Value::as_object)
                    .and_then(request_usage)
                    .is_some_and(|usage| usage.may_reach_long_prompt_floor())
                {
                    return None;
                }
            }
            Some(_) => saw_stream_message = true,
            None => {}
        }
    }
    (saw_result && saw_stream_message).then(Vec::new)
}

/// `model` without a trailing context-window marker like `[1m]`.
fn base_model(model: &str) -> &str {
    model
        .strip_suffix(']')
        .and_then(|rest| rest.rfind('[').map(|open| rest[..open].trim_end()))
        .unwrap_or(model)
}

/// Window length, in minutes, that a Claude window label names.
fn window_minutes(window: &str) -> Option<u32> {
    match window {
        "five_hour" => Some(5 * 60),
        _ if window.starts_with("seven_day") => Some(7 * 24 * 60),
        _ => None,
    }
}

/// The model family a Claude window is scoped to.
fn window_model(window: &str) -> Option<String> {
    window
        .strip_prefix("seven_day_")
        .filter(|family| matches!(*family, "opus" | "sonnet"))
        .map(str::to_string)
}

/// The usage windows the last `rate_limit_event` on `stdout` reports, one
/// observation per window, read at `now`.
///
/// `unifiedWindows` is preferred: it tracks every subscription window on each
/// event. The top-level window, which names the window currently limiting,
/// adds a reading only for a window `unifiedWindows` lacks. A window without
/// a `utilization` records nothing.
pub(crate) fn claude_usage_windows(
    stdout: &[u8],
    now: DateTime<Utc>,
) -> Vec<ProviderLimitObservation> {
    let Some(info) = top_level_frames(stdout)
        .filter(|(_, frame)| frame.get("type").and_then(Value::as_str) == Some("rate_limit_event"))
        .filter_map(|(_, frame)| frame.get("rate_limit_info")?.as_object().cloned())
        .last()
    else {
        return Vec::new();
    };
    let status = info.get("status").and_then(Value::as_str);
    let limiting = info.get("rateLimitType").and_then(Value::as_str);
    let detail = ["status", "rateLimitType"]
        .iter()
        .filter_map(|key| Some(format!("{key}={}", info.get(*key)?.as_str()?)))
        .collect::<Vec<_>>()
        .join(" ");
    let reading = |window: &str, fields: &Map<String, Value>| {
        let used_percent = percent(fields.get("utilization")?.as_f64()? * 100.0);
        Some(ProviderLimitObservation {
            provider: "claude".to_string(),
            account: None,
            model: window_model(window),
            window: Some(window.to_string()),
            exhausted: status == Some("rejected") && limiting == Some(window),
            source: ProviderLimitSource::Event,
            resets_at: fields
                .get("resetsAt")
                .and_then(Value::as_i64)
                .and_then(|epoch| DateTime::from_timestamp(epoch, 0)),
            observed_at: now,
            run_id: None,
            crew: None,
            detail: ProviderLimitObservation::bounded_detail(&detail),
            used_percent: Some(used_percent),
            window_minutes: window_minutes(window),
            gating: !window.contains("overage"),
            partial: false,
        })
    };

    let unified = info.get("unifiedWindows").and_then(Value::as_object);
    let mut readings: Vec<_> = unified
        .into_iter()
        .flatten()
        .filter_map(|(window, fields)| reading(window, fields.as_object()?))
        .collect();
    if let Some(window) = limiting
        && !unified.is_some_and(|windows| windows.contains_key(window))
    {
        readings.extend(reading(window, &info));
    }
    readings
}
