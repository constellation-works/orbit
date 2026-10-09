//! The implementer's `unfiled_findings` output field [ORB-14927].
//!
//! `agent_implement` declares `unfiled_findings` as an array of
//! `{title, description}` objects (extra keys such as `relations` allowed). The
//! engine does not enforce `output_schema_json`, so this module is the
//! enforcement at two points:
//!
//! - the implement step boundary calls [`unfiled_findings_shape_error`] and
//!   fails the step while nothing has been committed, pushed or opened;
//! - the claim handoff calls [`normalize_unfiled_findings`], which also
//!   accepts a plain string entry and rewrites it as a finding object, so a
//!   candidate already published with string findings can still be handed off.

use serde_json::{Map, Value, json};

/// Longest title [`normalize_unfiled_findings`] derives from a string entry.
const MAX_DERIVED_TITLE_BYTES: usize = 120;

/// Marker key on a finding object that [`normalize_unfiled_findings`] built
/// from a plain string, so the owner can see the shape was repaired.
const NORMALIZED_FINDING_KEY: &str = "normalized_from";

/// Why the implementer's `unfiled_findings` breaks its declared shape, or
/// `None` when it is absent, `null`, or an array of `{title, description}`
/// objects with non-blank string values.
#[must_use]
pub fn unfiled_findings_shape_error(output: &Value) -> Option<String> {
    let findings = output.get("unfiled_findings").filter(|v| !v.is_null())?;
    let Some(entries) = findings.as_array() else {
        return Some(format!(
            "the implementer's `unfiled_findings` must be an array of {{title, description}} \
             objects, not {}",
            kind(findings)
        ));
    };
    entries.iter().enumerate().find_map(|(index, entry)| {
        let Some(object) = entry.as_object() else {
            return Some(format!(
                "`unfiled_findings[{index}]` is {}; each entry must be an object with a string \
                 `title` and `description`",
                kind(entry)
            ));
        };
        ["title", "description"].into_iter().find_map(|field| {
            let present = object
                .get(field)
                .and_then(Value::as_str)
                .is_some_and(|text| !text.trim().is_empty());
            (!present)
                .then(|| format!("`unfiled_findings[{index}]` needs a non-empty string `{field}`"))
        })
    })
}

/// The handoff's reading of `unfiled_findings`: object entries verbatim, and
/// each non-blank string entry rewritten as
/// `{title: <first sentence>, description: <the string>, normalized_from: "string"}`.
///
/// Returns the findings and how many string entries were rewritten. Anything
/// else (a non-array, a blank string, a number, `null` inside the array) is an
/// error, so the handoff's other checks are not loosened.
pub fn normalize_unfiled_findings(findings: &Value) -> Result<(Vec<Value>, usize), String> {
    let entries = findings.as_array().ok_or_else(|| {
        "the implementer's `unfiled_findings` must be an array of finding objects".to_string()
    })?;
    let mut normalized = 0;
    let mut out = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        match entry {
            Value::Object(_) => out.push(entry.clone()),
            Value::String(text) if !text.trim().is_empty() => {
                let text = text.trim();
                let mut object = Map::new();
                object.insert("title".into(), json!(first_sentence(text)));
                object.insert("description".into(), json!(text));
                object.insert(NORMALIZED_FINDING_KEY.into(), json!("string"));
                out.push(Value::Object(object));
                normalized += 1;
            }
            other => {
                return Err(format!(
                    "`unfiled_findings[{index}]` is {}; the handoff accepts a finding object or \
                     a non-blank string",
                    kind(other)
                ));
            }
        }
    }
    Ok((out, normalized))
}

/// The text up to the first sentence end (`.`, `!` or `?` before whitespace)
/// or line break, cut to [`MAX_DERIVED_TITLE_BYTES`] on a char boundary.
fn first_sentence(text: &str) -> String {
    let line = text.lines().next().unwrap_or(text);
    let mut end = line.len();
    let mut chars = line.char_indices().peekable();
    while let Some((at, ch)) = chars.next() {
        if matches!(ch, '.' | '!' | '?')
            && chars.peek().is_none_or(|(_, next)| next.is_whitespace())
        {
            end = at + ch.len_utf8();
            break;
        }
    }
    let sentence = &line[..end];
    if sentence.len() <= MAX_DERIVED_TITLE_BYTES {
        return sentence.to_string();
    }
    let mut cut = MAX_DERIVED_TITLE_BYTES;
    while !sentence.is_char_boundary(cut) {
        cut -= 1;
    }
    sentence[..cut].trim_end().to_string()
}

fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}
