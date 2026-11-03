//! Provider-owned stdout frames and structured failures.

use orbit_agent::{provider_authentication_failure, provider_content_refusal};
use serde_json::Value;

/// Stdout's JSON frames, for the provider-owned failure readers below.
///
/// A slice `StreamDeserializer` truncates its input at the first parse error,
/// so a stray non-JSON line or a lone-surrogate escape would hide every later
/// frame. Parse one value at a time to preserve valid multiline frames. On an
/// error, skip the malformed record's first line, since the parser may have
/// read into a following valid frame before discovering the error.
pub(in super::super) fn stdout_frames(stdout: &[u8]) -> impl Iterator<Item = Value> + '_ {
    let mut rest = stdout;
    std::iter::from_fn(move || {
        loop {
            rest = rest.trim_ascii_start();
            let mut stream = serde_json::Deserializer::from_slice(rest).into_iter::<Value>();
            match stream.next()? {
                Ok(frame) => {
                    rest = &rest[stream.byte_offset()..];
                    return Some(frame);
                }
                Err(_) => {
                    rest = rest
                        .iter()
                        .position(|byte| *byte == b'\n')
                        .map_or(&[], |newline| &rest[newline + 1..]);
                }
            }
        }
    })
}

/// The failure a provider wrote in its own control-plane `frame`, never
/// assistant or tool text, and never an Orbit envelope, which describes the
/// work rather than the provider.
pub(in super::super) fn provider_failure<'a>(
    provider: &str,
    frame: &'a Value,
) -> Option<&'a Value> {
    if frame.get("schemaVersion").is_some() {
        return None;
    }
    match provider {
        "claude"
            if frame.get("is_error").and_then(Value::as_bool) == Some(true)
                && matches!(
                    frame.get("type").and_then(Value::as_str),
                    None | Some("result")
                ) =>
        {
            Some(frame)
        }
        "codex"
            if matches!(
                frame.get("type").and_then(Value::as_str),
                Some("error" | "turn.failed")
            ) =>
        {
            Some(frame.get("error").unwrap_or(frame))
        }
        "grok" | "gemini"
            if matches!(
                frame.get("type").and_then(Value::as_str),
                None | Some("error")
            ) =>
        {
            frame.get("error")
        }
        // [ORB-16597] Muse frames key on `payload_type`, not `type`: the
        // terminal record is `run.terminal.completed` whatever its outcome,
        // and `payload.terminal` carries the wire-open outcome. Only a
        // non-completed terminal is a provider failure frame; its payload
        // (`terminal`, `text`, and any status fields it carries) is the
        // provider-owned failure. Prompt echoes, deltas and child-task
        // records carry other `payload_type`s and never match.
        "muse"
            if frame
                .get("payload_type")
                .and_then(Value::as_str)
                .is_some_and(|payload_type| payload_type.starts_with("run.terminal."))
                && frame
                    .get("payload")
                    .and_then(|payload| payload.get("terminal"))
                    .and_then(Value::as_str)
                    != Some("completed") =>
        {
            Some(frame.get("payload").unwrap_or(frame))
        }
        _ => None,
    }
}

/// Read only provider-owned failure frames, never assistant or tool text.
/// Claude can emit an error result even with exit 0; the wrapper must still
/// fail the invocation rather than allowing an earlier answer to succeed.
pub(super) fn structured_provider_auth_error(provider: &str, stdout: &[u8]) -> Option<String> {
    stdout_frames(stdout).find_map(|frame| {
        let failure = provider_failure(provider, &frame)?;
        // Provider-owned field sets that may carry the status or the prose:
        // the failure itself, its frame, and a nested `error` object. For
        // Muse the failure is the terminal `payload`, so its `text` and any
        // status fields it carries are read here. [ORB-16597]
        let mut sources: Vec<&Value> = vec![failure, &frame];
        if let Some(error) = failure.get("error").filter(|error| error.is_object()) {
            sources.push(error);
        }
        let status = sources.iter().find_map(|fields| {
            [
                "api_error_status",
                "status",
                "status_code",
                "http_status",
                "httpStatus",
                "code",
            ]
            .iter()
            .find_map(|key| {
                let value = fields.get(*key)?;
                let status = value.as_u64().or_else(|| value.as_str()?.parse().ok())?;
                matches!(status, 401 | 403).then_some(status)
            })
        });
        let prose: Vec<&str> = failure
            .as_str()
            .into_iter()
            .chain(sources.iter().flat_map(|fields| {
                ["message", "text", "result", "error"]
                    .iter()
                    .filter_map(|key| fields.get(*key).and_then(Value::as_str))
                    .collect::<Vec<_>>()
            }))
            .chain(
                ["message", "result", "type", "status", "code"]
                    .iter()
                    .filter_map(|key| failure.get(*key).and_then(Value::as_str)),
            )
            .filter(|text| !text.trim().is_empty())
            .collect();
        let auth_message = prose
            .iter()
            .copied()
            .find(|text| provider_authentication_failure(text));
        // With an HTTP 401/403 the status itself is the evidence, so the
        // provider's own prose is carried even when it names no known
        // authentication phrase.
        let message = auth_message.or_else(|| status.and_then(|_| prose.first().copied()));
        if status.is_none() && auth_message.is_none() {
            return None;
        }
        let status = status
            .map(|status| format!(" (HTTP {status})"))
            .unwrap_or_default();
        Some(format!(
            "{provider} provider authentication failure{status}: {}",
            message.unwrap_or("provider credentials were rejected")
        ))
    })
}

/// [ORB-14266] Claude's terminal `result` frame stopping for a refusal: the
/// provider declined the turn, so no answer before it may stand. Only the
/// provider's own control frame is read, never assistant or tool text.
pub(super) fn structured_provider_refusal(provider: &str, stdout: &[u8]) -> Option<String> {
    if provider != "claude" {
        return None;
    }
    let frame = stdout_frames(stdout)
        .filter(|frame| frame.get("schemaVersion").is_none())
        .filter(|frame| frame.get("type").and_then(Value::as_str) == Some("result"))
        .last()?;
    let refused = frame.get("stop_reason").and_then(Value::as_str) == Some("refusal")
        || (frame.get("is_error").and_then(Value::as_bool) == Some(true)
            && frame
                .get("result")
                .and_then(Value::as_str)
                .is_some_and(provider_content_refusal));
    refused.then(|| {
        frame
            .get("result")
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
            .unwrap_or("stop_reason=refusal")
            .to_string()
    })
}

/// The text a failed provider wrote about itself that `matches` — its
/// stderr, its terminal error, or a provider-owned failure frame on stdout.
/// Codex reports an exhausted model [ORB-14149] and a content-filter refusal
/// [ORB-14266] only as `error` and `turn.failed` frames.
pub(super) fn provider_reported(
    provider: &str,
    stdout: &[u8],
    stderr_text: &str,
    terminal_error: Option<&str>,
    matches: fn(&str) -> bool,
) -> Option<String> {
    let frame_text = || {
        stdout_frames(stdout).find_map(|frame| {
            let failure = provider_failure(provider, &frame)?;
            failure
                .as_str()
                .into_iter()
                .chain(
                    ["message", "result", "text", "error"]
                        .iter()
                        .filter_map(|key| failure.get(*key).and_then(Value::as_str)),
                )
                .chain(failure.get("error").into_iter().flat_map(|error| {
                    ["message", "text"]
                        .iter()
                        .filter_map(|key| error.get(*key).and_then(Value::as_str))
                        .collect::<Vec<_>>()
                }))
                .find(|text| matches(text))
                .map(str::to_string)
        })
    };
    let line = |text: &str| {
        text.lines()
            .find(|line| matches(line))
            .map(|line| line.trim().to_string())
    };
    line(stderr_text)
        .or_else(|| terminal_error.and_then(line))
        .or_else(frame_text)
}
