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
//! - Partial message events pair each request's starting input buckets with
//!   its final output for prompt-size pricing. Invocation totals still come
//!   exclusively from the terminal `result`.

use chrono::{DateTime, Utc};
use orbit_types::telemetry::{ProviderLimitObservation, ProviderLimitSource, TokenUsage};
use serde_json::{Map, Value};

use orbit_types::telemetry::ToolCallTrace;

use super::super::usage_window::{percent, top_level_frames};
use crate::types::{request_usage, tool_calls_in_documents};

/// Tool calls on a Claude message stream.
///
/// `None` for a single `--output-format json` result document, which has no
/// stream of tool frames. The answer and usage projections stay on the
/// terminal `result`; this reads the raw stream the projection drops.
pub(crate) fn recorded_claude_tool_calls(stdout: &[u8]) -> Option<Vec<ToolCallTrace>> {
    let frames: Vec<Value> = top_level_frames(stdout).map(|(_, frame)| frame).collect();
    let stream = frames.iter().any(|frame| {
        matches!(
            frame.get("type").and_then(Value::as_str),
            Some(kind) if kind != "result"
        )
    });
    stream.then(|| tool_calls_in_documents(&frames))
}

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

/// Final usage of each possibly-long request on the invocation's model.
/// Assistant content blocks repeat starting usage, not final output. Partial
/// message events supply the final output, correlated by `api_message_id`.
/// Missing request completion, terminal result or stream capture stays unknown.
pub(crate) fn claude_long_prompt_requests(
    stdout: &[u8],
    model: Option<&str>,
) -> Option<Vec<TokenUsage>> {
    let mut requests = std::collections::BTreeMap::<String, ClaudeRequestUsage>::new();
    let mut saw_result = false;
    let mut saw_stream_message = false;
    for (_, frame) in top_level_frames(stdout) {
        match frame.get("type").and_then(Value::as_str) {
            Some("result") => saw_result = true,
            Some("assistant") => {
                saw_result = false;
                saw_stream_message = true;
                record_request_start(&mut requests, frame.get("message")?, model, false)?;
            }
            Some("stream_event") => {
                saw_result = false;
                saw_stream_message = true;
                let event = frame.get("event")?;
                match event.get("type").and_then(Value::as_str) {
                    Some("message_start") => {
                        let message = event.get("message")?;
                        // An explicit correlation key must agree with the start.
                        if let Some(id) = frame.get("api_message_id")
                            && Some(id) != message.get("id")
                        {
                            return None;
                        }
                        record_request_start(&mut requests, message, model, true)?;
                    }
                    Some("message_delta" | "message_stop") => {
                        let id = frame.get("api_message_id")?.as_str()?;
                        let request = requests.get_mut(id)?;
                        if request.excluded {
                            continue;
                        }
                        if event.get("type").and_then(Value::as_str) == Some("message_stop") {
                            request.stopped = true;
                        } else {
                            if request.stopped {
                                return None;
                            }
                            let usage = event.get("usage")?.as_object()?;
                            let output = usage.get("output_tokens")?.as_u64()?;
                            if output < request.usage.output {
                                return None;
                            }
                            // Input buckets are final at message_start. Refuse
                            // a contradictory delta rather than mix two versions.
                            for (key, expected) in [
                                ("input_tokens", request.usage.input),
                                ("cache_read_input_tokens", request.usage.cache_read),
                                (
                                    "cache_creation_input_tokens",
                                    request
                                        .usage
                                        .cache_create
                                        .checked_add(request.usage.cache_create_1h)?,
                                ),
                            ] {
                                if usage
                                    .get(key)
                                    .is_some_and(|value| value.as_u64() != Some(expected))
                                {
                                    return None;
                                }
                            }
                            if event.get("delta")?.get("stop_reason")?.as_str().is_some() {
                                request.final_output = Some(output);
                            }
                        }
                    }
                    _ => {}
                }
            }
            Some(_) => saw_stream_message = true,
            None => {}
        }
    }
    if !saw_result || !saw_stream_message {
        return None;
    }
    requests
        .into_values()
        .filter(|request| !request.excluded && request.usage.may_reach_long_prompt_floor())
        .map(|request| {
            if !request.started || !request.stopped || !request.cache_ttl_split_complete {
                return None;
            }
            Some(TokenUsage {
                output: request.final_output?,
                ..request.usage
            })
        })
        .collect()
}

struct ClaudeRequestUsage {
    usage: TokenUsage,
    excluded: bool,
    started: bool,
    stopped: bool,
    cache_ttl_split_complete: bool,
    final_output: Option<u64>,
}

fn record_request_start(
    requests: &mut std::collections::BTreeMap<String, ClaudeRequestUsage>,
    message: &Value,
    model: Option<&str>,
    started: bool,
) -> Option<()> {
    let id = message.get("id")?.as_str()?;
    let request_model = message.get("model").and_then(Value::as_str);
    let excluded = model
        .zip(request_model)
        .is_some_and(|(model, request)| base_model(model) != base_model(request));
    let (usage, cache_ttl_split_complete) = if excluded {
        (TokenUsage::default(), true)
    } else {
        let fields = message.get("usage")?.as_object()?;
        // Missing buckets are not zero: they cannot bound the prompt.
        for key in [
            "input_tokens",
            "output_tokens",
            "cache_read_input_tokens",
            "cache_creation_input_tokens",
        ] {
            fields.get(key)?.as_u64()?;
        }
        if let Some(split) = fields.get("cache_creation") {
            let split = split.as_object()?;
            for key in ["ephemeral_5m_input_tokens", "ephemeral_1h_input_tokens"] {
                if let Some(count) = split.get(key) {
                    count.as_u64()?;
                }
            }
        }
        let usage = request_usage(fields)?;
        if usage.cache_create.checked_add(usage.cache_create_1h)?
            != fields.get("cache_creation_input_tokens")?.as_u64()?
        {
            return None;
        }
        let cache_creation_split_present = fields
            .get("cache_creation")
            .and_then(Value::as_object)
            .is_some_and(|split| {
                split.contains_key("ephemeral_5m_input_tokens")
                    || split.contains_key("ephemeral_1h_input_tokens")
            });
        let cache_ttl_split_complete = cache_creation_split_present || usage.cache_create == 0;
        (usage, cache_ttl_split_complete)
    };
    if let Some(request) = requests.get_mut(id) {
        if request.usage != usage || request.excluded != excluded {
            return None;
        }
        request.started |= started;
        request.cache_ttl_split_complete |= cache_ttl_split_complete;
    } else {
        requests.insert(
            id.to_string(),
            ClaudeRequestUsage {
                usage,
                excluded,
                started,
                stopped: false,
                cache_ttl_split_complete,
                final_output: None,
            },
        );
    }
    Some(())
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
