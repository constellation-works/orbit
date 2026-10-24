//! Reducing Muse Code's `muse exec --json` JSONL stream to the bytes Orbit's
//! response/envelope contract is allowed to read. [ORB-16577]
//!
//! With `--json`, every stdout line is one record with a `payload_type`, and
//! the run ends with a `run.terminal.completed` record (Muse Code 1.4.10):
//!
//! ```text
//! {"payload_type":"run.lifecycle.started","payload":{…, "prompt":"…"}}
//! {"payload_type":"turn.input.user","payload":{"text":"…"}}
//! {"payload_type":"run.output.delta","payload":{"text":"…"}}
//! {"payload_type":"run.terminal.completed","payload":{"terminal":"completed","text":"…"}}
//! ```
//!
//! Handed the raw stream, Orbit's reverse envelope search is unsafe:
//! `run.lifecycle.started` and `turn.input.user` echo Orbit's own prompt
//! verbatim, including the literal example envelope in its response
//! contract; `run.output.delta` carries partial text; and child-task records
//! (reminder agents) report their own outcomes. None of them is the run's
//! answer.
//!
//! So the answer projection ([`project_muse_response`]) keeps only
//! `payload.text` of the last `run.terminal.completed` record, and only when
//! its `payload.terminal` is `completed`. The exported schema marks
//! `TurnTerminal` wire-open, so `failed`, `cancelled`, and any value Orbit
//! does not know are all failures.
//!
//! A non-completed terminal's `payload.text` (falling back to `payload.message`,
//! `payload.error`, or `payload.error.message`) is provider-owned failure
//! prose, not an answer: normalization retains it for invocation traces and
//! operator diagnostics (see [`muse_terminal_failure_diagnostic`]), while
//! the answer projection above still reduces it to empty, so it can never
//! satisfy the completion envelope. Classification of that prose (say, as
//! an authentication failure) stays with the engine's shared
//! provider-failure machinery, never with this reducer.
//! Every stdout line must be a record: a line that is not a JSON object with
//! a string `payload_type` means the stream cannot be trusted, and the whole
//! output reduces to empty, which carries no envelope and so fails closed.

use chrono::{DateTime, Utc};
use orbit_types::telemetry::{ProviderLimitObservation, ProviderLimitSource};
use serde_json::Value;

use super::super::usage_window::top_level_frames;

/// The one record type whose payload carries the run's terminal outcome.
const TERMINAL_RECORD: &str = "run.terminal.completed";

/// The only `TurnTerminal` value that means the run produced its answer.
const COMPLETED: &str = "completed";

/// Upper bound on provider failure prose copied into a diagnostic. The CLI
/// runner bounds and redacts again at its own boundary.
const TERMINAL_ERROR_LIMIT_CHARS: usize = 400;

/// The last terminal record's provider-owned fields Orbit reads:
/// `payload.terminal` (the outcome) and `payload.text` (the final answer
/// when completed, the provider's failure reason when not).
struct TerminalInfo {
    terminal: Option<String>,
    text: Option<String>,
}

impl TerminalInfo {
    fn is_completed(&self) -> bool {
        self.terminal.as_deref() == Some(COMPLETED)
    }

    fn text(&self) -> Option<&str> {
        self.text
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
    }
}

/// The last terminal record in the stream, or `None` when there is none or
/// the stream is malformed (a line that is not a JSON object with a string
/// `payload_type` poisons the whole stream).
fn last_terminal(stdout: &[u8]) -> Option<TerminalInfo> {
    let text = String::from_utf8_lossy(stdout);
    let mut terminal = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<Value>(trimmed) else {
            return None;
        };
        let payload_type = record.get("payload_type").and_then(Value::as_str)?;
        if payload_type == TERMINAL_RECORD || payload_type.starts_with("run.terminal.") {
            // A later terminal record replaces an earlier one, so a failed
            // final record cannot be outvoted by a completed earlier one.
            terminal = Some(terminal_info(record.get("payload")));
        }
    }
    terminal
}

fn terminal_info(payload: Option<&Value>) -> TerminalInfo {
    let Some(payload) = payload.and_then(Value::as_object) else {
        return TerminalInfo {
            terminal: None,
            text: None,
        };
    };
    let info = TerminalInfo {
        terminal: payload
            .get("terminal")
            .and_then(Value::as_str)
            .map(str::to_string),
        text: payload
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_string),
    };
    // A completed answer comes exclusively from string `payload.text`.
    // Diagnostic fallbacks must never supply an answer when it is missing
    // or malformed, even when they quote a valid Orbit response envelope.
    if info.is_completed() {
        return info;
    }
    TerminalInfo {
        terminal: info.terminal,
        text: info
            .text
            .or_else(|| {
                payload
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .or_else(|| match payload.get("error") {
                Some(Value::String(text)) => Some(text.clone()),
                Some(error) => error
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                None => None,
            }),
    }
}

/// Return the terminal record's text for tracing and diagnostics,
/// or empty bytes when the stream is malformed or carries no terminal text.
///
/// For a completed run this is the final answer. For a failed, cancelled or
/// unknown terminal it is the provider's own failure reason, including
/// `message`/`error` fallbacks when `text` is absent or non-string, retained only
/// so the operator diagnostic can carry it; the answer projection is
/// [`project_muse_response`], which never exposes it as an answer.
pub(crate) fn normalize_muse_stdout(stdout: &[u8]) -> Vec<u8> {
    let Some(info) = last_terminal(stdout) else {
        return Vec::new();
    };
    info.text()
        .map_or_else(Vec::new, |text| text.as_bytes().to_vec())
}

/// Return the final answer text of a completed Muse run, or empty bytes.
///
/// Only a `payload.terminal` of `completed` yields its `payload.text`; every
/// other terminal, a missing terminal record, and a malformed stream reduce
/// to empty, which carries no envelope and so fails closed.
pub(crate) fn project_muse_response(stdout: &[u8]) -> Vec<u8> {
    let Some(info) = last_terminal(stdout) else {
        return Vec::new();
    };
    if !info.is_completed() {
        return Vec::new();
    }
    info.text()
        .map_or_else(Vec::new, |text| text.as_bytes().to_vec())
}

/// Provider-gated diagnostic for a Muse run whose terminal is not
/// `completed`: the provider's own failure reason from the terminal frame's
/// `payload.text` or its `message`/`error` fallbacks, read from no other record,
/// so prompt echoes, deltas and
/// child-task records never count as failure evidence. Completed terminals
/// and malformed streams yield `None`. Unbounded in the prose it selects;
/// the caller bounds and redacts it, and this helper bounds it again.
pub fn muse_terminal_failure_diagnostic(provider: &str, stdout: &[u8]) -> Option<String> {
    if provider != "muse" {
        return None;
    }
    let info = last_terminal(stdout)?;
    if info.is_completed() {
        return None;
    }
    let terminal = info.terminal.as_deref().unwrap_or("unknown");
    let text = match info.text() {
        Some(text) => format!("Muse terminal {terminal}: {text}"),
        None => format!("Muse run ended with terminal {terminal} and no failure text"),
    };
    Some(bound_terminal_error(&text))
}

fn bound_terminal_error(text: &str) -> String {
    let bounded: String = text.chars().take(TERMINAL_ERROR_LIMIT_CHARS).collect();
    if bounded.chars().count() < text.chars().count() {
        format!("{bounded}…")
    } else {
        bounded
    }
}

/// Account windows from the last top-level `status` / `run_usage` allowance
/// (Muse Code 1.4.12), observed at run completion. Prompt echoes, answer text,
/// tool results and child-task records never supply account telemetry.
/// Preserve the provider's labels; normalize the weekly allowance to seven days.
pub(crate) fn muse_usage_windows(
    stdout: &[u8],
    now: DateTime<Utc>,
) -> Vec<ProviderLimitObservation> {
    let Some(allowance) = top_level_frames(stdout)
        .filter(|(_, frame)| frame.get("payload_type").and_then(Value::as_str) == Some("status"))
        .filter_map(|(_, frame)| {
            let payload = frame.get("payload")?;
            if payload.get("kind").and_then(Value::as_str) != Some("run_usage") {
                return None;
            }
            payload.get("allowance")?.as_object().cloned()
        })
        .last()
    else {
        return Vec::new();
    };
    ["window", "weekly"]
        .into_iter()
        .filter_map(|window| {
            let fields = allowance.get(window)?.as_object()?;
            let used_percent = fields.get("used_percent")?.as_f64()?;
            if !used_percent.is_finite() || used_percent < 0.0 {
                return None;
            }
            let resets_at = DateTime::from_timestamp_millis(fields.get("resets_at_ms")?.as_i64()?)?;
            Some(ProviderLimitObservation {
                provider: "muse".to_string(),
                account: None,
                model: None,
                window: Some(window.to_string()),
                exhausted: used_percent >= 100.0,
                source: ProviderLimitSource::Event,
                resets_at: Some(resets_at),
                used_percent: Some(used_percent),
                window_minutes: (window == "weekly").then_some(10080),
                gating: true,
                partial: false,
                observed_at: now,
                run_id: None,
                crew: None,
                detail: "kind=run_usage".to_string(),
            })
        })
        .collect()
}
