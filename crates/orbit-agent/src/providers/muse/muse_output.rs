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
//! So this module keeps only `payload.text` of the last
//! `run.terminal.completed` record, and only when its `payload.terminal` is
//! `completed`. The exported schema marks `TurnTerminal` wire-open, so
//! `failed`, `cancelled`, and any value Orbit does not know are all failures.
//! Every stdout line must be a record: a line that is not a JSON object with
//! a string `payload_type` means the stream cannot be trusted, and the whole
//! output reduces to empty, which carries no envelope and so fails closed.

use serde_json::Value;

/// The one record type whose payload is the run's final answer.
const TERMINAL_RECORD: &str = "run.terminal.completed";

/// The only `TurnTerminal` value that means the run produced its answer.
const COMPLETED: &str = "completed";

/// Return the final answer text of a completed Muse run, or empty bytes.
pub(crate) fn normalize_muse_stdout(stdout: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(stdout);
    let mut terminal = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<Value>(trimmed) else {
            return Vec::new();
        };
        let Some(payload_type) = record.get("payload_type").and_then(Value::as_str) else {
            return Vec::new();
        };
        if payload_type == TERMINAL_RECORD {
            // A later terminal record replaces an earlier one, so a failed
            // final record cannot be outvoted by a completed earlier one.
            terminal = Some(completed_answer(record.get("payload")));
        }
    }
    terminal.flatten().map_or_else(Vec::new, String::into_bytes)
}

/// The answer of one terminal record, or `None` when it is not completed or
/// carries no text.
fn completed_answer(payload: Option<&Value>) -> Option<String> {
    let payload = payload?.as_object()?;
    if payload.get("terminal").and_then(Value::as_str) != Some(COMPLETED) {
        return None;
    }
    payload
        .get("text")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}
