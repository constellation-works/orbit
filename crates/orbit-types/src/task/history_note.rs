//! One-line human summary of a JSON task history note.
//!
//! A history note often holds a machine record: schema ids, run and operation
//! ids, digests. Only human-oriented fields are eligible for a summary, so a
//! timeline row never shows bookkeeping. The stored note is never rewritten.
//! The dashboard's `historyJsonSummary` (`task-history.js`) mirrors this
//! allowlist and filter; change both together.

use serde_json::Value;

/// Longest summary kept, in characters, before it is cut with an ellipsis.
const SUMMARY_MAX_CHARS: usize = 75;

/// What a JSON object note with no human-oriented field summarizes to.
const STRUCTURED_NOTE: &str = "structured note";

/// String fields shown as-is, in this order.
const TEXT_FIELDS: [&str; 11] = [
    "summary",
    "disposition",
    "verdict",
    "blocker",
    "detail",
    "tool",
    "phase",
    "ref",
    "outcome",
    "status",
    "code",
];

/// The summary of a history note: a JSON object note reduces to its
/// human-oriented fields, any other note is returned unchanged.
///
/// Arbitrary scalar fields can hold ledger keys, identifiers or hashes, so
/// only `summary`, `disposition`, `verdict`, `blocker`, `detail`, `tool`,
/// `phase`, `ref`, `outcome`, `status`, `code`, `classification` and
/// `attempts` are eligible (the last two, the retry-exhaustion fields, with
/// their labels). Words containing `sha256` or a 32+ character hex run are
/// dropped.
pub fn history_note_summary(note: &str) -> String {
    let Ok(Value::Object(object)) = serde_json::from_str::<Value>(note) else {
        return note.to_owned();
    };
    let words = |key: &str| {
        object
            .get(key)
            .and_then(Value::as_str)
            .map(history_text_words)
            .filter(|value| !value.is_empty())
    };
    let mut parts: Vec<String> = TEXT_FIELDS.iter().copied().filter_map(words).collect();
    parts.extend(words("classification").map(|value| format!("classification={value}")));
    parts.extend(
        object
            .get("attempts")
            .and_then(Value::as_u64)
            .map(|attempts| format!("attempts={attempts}")),
    );
    let summary = if parts.is_empty() {
        STRUCTURED_NOTE.to_owned()
    } else {
        parts.join(" · ")
    };
    let mut chars = summary.chars();
    let prefix: String = chars.by_ref().take(SUMMARY_MAX_CHARS).collect();
    if chars.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

/// Keeps the words of a note field that are not digests or long hex identifiers.
fn history_text_words(value: &str) -> String {
    value
        .split_whitespace()
        .filter(|word| {
            let lower = word.to_ascii_lowercase();
            !lower.contains("sha256")
                && !word
                    .split(|c: char| !c.is_ascii_hexdigit())
                    .any(|part| part.len() >= 32)
        })
        .collect::<Vec<_>>()
        .join(" ")
}
