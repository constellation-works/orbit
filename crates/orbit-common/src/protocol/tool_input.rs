use serde_json::Value;

use crate::OrbitError;

pub const RETIRED_TASK_ADD_INPUT_FIELDS: &[&str] = &[
    "plan",
    "status",
    "parent_id",
    "source_task_id",
    "external_refs",
    "context",
    "comment",
    "dependencies",
];

/// Top-level keys that are transport/session wrappers, not tool arguments.
///
/// MCP `_meta` and the workspace routing selector may appear beside the
/// advertised parameters. They stay allowed even when the tool's JSON Schema
/// sets `additionalProperties: false` on the argument object.
pub const TOOL_INPUT_TRANSPORT_WRAPPER_KEYS: &[&str] = &["_meta", "workspace"];

/// Refuse unknown top-level tool-argument keys with a did-you-mean hint.
///
/// Transport wrappers in [`TOOL_INPUT_TRANSPORT_WRAPPER_KEYS`] are ignored.
/// The first unknown key is reported; a close match against `allowed` is
/// included in the message and on [`OrbitError::did_you_mean`].
pub fn reject_unknown_tool_fields(input: &Value, allowed: &[&str]) -> Result<(), OrbitError> {
    let Some(object) = input.as_object() else {
        return Ok(());
    };

    let unknown = object
        .keys()
        .filter(|key| {
            let key = key.as_str();
            !TOOL_INPUT_TRANSPORT_WRAPPER_KEYS.contains(&key) && !allowed.contains(&key)
        })
        .cloned()
        .collect::<Vec<_>>();
    if unknown.is_empty() {
        return Ok(());
    }

    let first = &unknown[0];
    let suggestion = suggest_tool_field(first, allowed);
    let message = unknown_tool_field_message(&unknown, suggestion);
    Err(OrbitError::invalid_input_with_suggestions(
        message,
        suggestion
            .map(|name| vec![name.to_string()])
            .unwrap_or_default(),
    ))
}

/// Refuse retired `orbit.task.add` fields that used to be stripped silently.
///
/// These names are not misspellings of add parameters; they belong on
/// `orbit.task.update` (or a later follow-up). The error names the field and
/// the tool that accepts it.
pub fn reject_retired_task_add_input_fields(input: &Value) -> Result<(), OrbitError> {
    let Some(object) = input.as_object() else {
        return Ok(());
    };

    for field in RETIRED_TASK_ADD_INPUT_FIELDS {
        if object.contains_key(*field) {
            return Err(OrbitError::InvalidInput(format!(
                "unknown field '{field}' (orbit.task.add does not accept '{field}'; \
                 set it with orbit.task.update)"
            )));
        }
    }
    Ok(())
}

fn unknown_tool_field_message(unknown: &[String], suggestion: Option<&str>) -> String {
    let hint = suggestion
        .map(|name| format!(" (did you mean '{name}'?)"))
        .unwrap_or_default();
    if unknown.len() == 1 {
        format!("unknown field '{}'{}", unknown[0], hint)
    } else {
        format!("unknown fields '{}'{}", unknown.join("', '"), hint)
    }
}

fn suggest_tool_field<'a>(unknown: &str, allowed: &[&'a str]) -> Option<&'a str> {
    let normalized = normalize_tool_field_name(unknown);
    if let Some(name) = allowed.iter().find(|name| **name == normalized) {
        return Some(*name);
    }
    if let Some(canonical) = synonym_tool_field(unknown).or_else(|| synonym_tool_field(&normalized))
        && allowed.contains(&canonical)
    {
        return Some(canonical);
    }

    let mut best: Option<(&'a str, usize)> = None;
    for name in allowed {
        let distance = levenshtein(unknown, name).min(levenshtein(&normalized, name));
        if best.is_none_or(|(_, best_distance)| distance < best_distance) {
            best = Some((*name, distance));
        }
    }
    best.and_then(|(name, distance)| {
        let threshold = unknown.len().max(name.len()).div_ceil(3).max(1);
        (distance <= threshold).then_some(name)
    })
}

fn synonym_tool_field(unknown: &str) -> Option<&'static str> {
    match unknown {
        "note" | "notes" | "message" | "msg" => Some("comment"),
        "deps" | "depends_on" | "depends-on" => Some("dependencies"),
        "acceptancecriteria" => Some("acceptance_criteria"),
        "contextfiles" => Some("context_files"),
        "requiredtools" => Some("required_tools"),
        _ => None,
    }
}

fn normalize_tool_field_name(raw: &str) -> String {
    let mut normalized = String::with_capacity(raw.len() + 4);
    for (index, ch) in raw.chars().enumerate() {
        if ch == '-' {
            if !normalized.ends_with('_') {
                normalized.push('_');
            }
            continue;
        }
        if ch.is_ascii_uppercase() {
            if index > 0 && !normalized.ends_with('_') {
                normalized.push('_');
            }
            normalized.push(ch.to_ascii_lowercase());
            continue;
        }
        normalized.push(ch);
    }
    normalized
}

fn levenshtein(left: &str, right: &str) -> usize {
    let right_chars = right.chars().collect::<Vec<_>>();
    let mut previous = (0..=right_chars.len()).collect::<Vec<_>>();
    let mut current = vec![0; right_chars.len() + 1];
    for (i, left_ch) in left.chars().enumerate() {
        current[0] = i + 1;
        for (j, right_ch) in right_chars.iter().enumerate() {
            let cost = usize::from(left_ch != *right_ch);
            current[j + 1] = (previous[j + 1] + 1)
                .min(current[j] + 1)
                .min(previous[j] + cost);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right_chars.len()]
}

pub fn required_string(
    input: &Value,
    keys: &[&str],
    canonical: &str,
) -> Result<String, OrbitError> {
    for key in keys {
        if let Some(value) = input.get(*key) {
            let raw = value
                .as_str()
                .ok_or_else(|| OrbitError::InvalidInput(format!("`{key}` must be a string")))?;
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Err(OrbitError::InvalidInput(format!(
                    "`{key}` must not be empty"
                )));
            }
            return Ok(trimmed.to_string());
        }
    }
    Err(OrbitError::InvalidInput(format!("missing `{canonical}`")))
}

pub fn optional_string(input: &Value, key: &str) -> Result<Option<String>, OrbitError> {
    match input.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let raw = value
                .as_str()
                .ok_or_else(|| OrbitError::InvalidInput(format!("`{key}` must be a string")))?;
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Err(OrbitError::InvalidInput(format!(
                    "`{key}` must not be empty"
                )));
            }
            Ok(Some(trimmed.to_string()))
        }
    }
}

pub fn optional_raw_string(input: &Value, key: &str) -> Result<Option<String>, OrbitError> {
    match input.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let raw = value
                .as_str()
                .ok_or_else(|| OrbitError::InvalidInput(format!("`{key}` must be a string")))?;
            Ok(Some(raw.to_string()))
        }
    }
}

pub fn optional_string_alias(input: &Value, keys: &[&str]) -> Result<Option<String>, OrbitError> {
    for key in keys {
        if let Some(value) = input.get(*key) {
            let raw = value
                .as_str()
                .ok_or_else(|| OrbitError::InvalidInput(format!("`{key}` must be a string")))?;
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Err(OrbitError::InvalidInput(format!(
                    "`{key}` must not be empty"
                )));
            }
            return Ok(Some(trimmed.to_string()));
        }
    }
    Ok(None)
}

pub fn optional_u32_alias(input: &Value, keys: &[&str]) -> Result<Option<u32>, OrbitError> {
    for key in keys {
        if let Some(value) = input.get(*key) {
            let raw = match value {
                Value::String(value) => value.trim().to_string(),
                Value::Number(value) => value.to_string(),
                _ => {
                    return Err(OrbitError::InvalidInput(format!(
                        "`{key}` must be a string or integer"
                    )));
                }
            };
            if raw.is_empty() {
                return Err(OrbitError::InvalidInput(format!(
                    "`{key}` must not be empty"
                )));
            }
            return raw.parse::<u32>().map(Some).map_err(|error| {
                OrbitError::InvalidInput(format!("`{key}` must be an unsigned integer: {error}"))
            });
        }
    }
    Ok(None)
}

pub fn optional_string_list_alias(
    input: &Value,
    keys: &[&str],
) -> Result<Option<Vec<String>>, OrbitError> {
    for key in keys {
        if let Some(value) = input.get(*key) {
            return match value {
                Value::String(raw) => {
                    let trimmed = raw.trim();
                    if trimmed.is_empty() {
                        Err(OrbitError::InvalidInput(format!(
                            "`{key}` must not be empty"
                        )))
                    } else if let Some(recovered) = decode_json_string_array(trimmed) {
                        Ok(Some(recovered))
                    } else {
                        Ok(Some(vec![trimmed.to_string()]))
                    }
                }
                Value::Array(items) => {
                    if let [Value::String(raw)] = items.as_slice()
                        && let Some(recovered) = decode_json_string_array(raw.trim())
                    {
                        return Ok(Some(recovered));
                    }
                    let mut values = Vec::with_capacity(items.len());
                    for item in items {
                        let raw = item.as_str().ok_or_else(|| {
                            OrbitError::InvalidInput(format!("`{key}` entries must be strings"))
                        })?;
                        let trimmed = raw.trim();
                        if trimmed.is_empty() {
                            return Err(OrbitError::InvalidInput(format!(
                                "`{key}` entries must not be empty"
                            )));
                        }
                        values.push(trimmed.to_string());
                    }
                    Ok(Some(values))
                }
                _ => Err(OrbitError::InvalidInput(format!(
                    "`{key}` must be a string or array of strings"
                ))),
            };
        }
    }
    Ok(None)
}

pub fn optional_csv_or_string_list_alias(
    input: &Value,
    keys: &[&str],
) -> Result<Option<Vec<String>>, OrbitError> {
    for key in keys {
        let Some(value) = input.get(*key) else {
            continue;
        };
        let values = optional_string_list_alias(input, &[*key])?;
        return Ok(values.map(|items| match value {
            Value::String(raw) if decode_json_string_array(raw.trim()).is_none() => items
                .into_iter()
                .flat_map(|item| split_csv(&item))
                .collect(),
            _ => items,
        }));
    }
    Ok(None)
}

pub fn split_csv(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

/// Recover a string array that an MCP client serialized as a JSON-encoded
/// scalar string. Some clients flatten arrays into JSON strings when a tool
/// schema is `anyOf:[array,string]`; without this recovery, the parser would
/// store the entire JSON blob as a single list element. Returns `Some(values)`
/// only when `raw` decodes to a JSON array of non-empty strings; otherwise
/// returns `None` so callers fall back to treating `raw` as plain text.
fn decode_json_string_array(raw: &str) -> Option<Vec<String>> {
    if !(raw.starts_with('[') && raw.ends_with(']')) {
        return None;
    }
    let parsed: Value = serde_json::from_str(raw).ok()?;
    let Value::Array(items) = parsed else {
        return None;
    };
    if items.is_empty() {
        return None;
    }
    let mut values = Vec::with_capacity(items.len());
    for item in items {
        let Value::String(text) = item else {
            return None;
        };
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return None;
        }
        values.push(trimmed.to_string());
    }
    Some(values)
}

/// Parse a bounded human duration (`30m`, `2h`, `1d`) into seconds.
///
/// One vocabulary for every surface that accepts a window: `s`, `m`, `h`,
/// `d`, and `w`. Empty, unitless, unknown-unit, and overflowing inputs are
/// refused with the offending text.
pub fn parse_duration_seconds(raw: &str) -> Result<u64, OrbitError> {
    let value = raw.trim();
    if value.is_empty() {
        return Err(OrbitError::InvalidInput(
            "duration must not be empty".to_string(),
        ));
    }
    let split_at = value
        .find(|c: char| c.is_alphabetic())
        .ok_or_else(|| OrbitError::InvalidInput(format!("invalid duration: {raw}")))?;
    let (number, unit) = value.split_at(split_at);
    let number: u64 = number
        .parse()
        .map_err(|_| OrbitError::InvalidInput(format!("invalid duration number: {raw}")))?;
    let seconds = match unit {
        "s" => Some(number),
        "m" => number.checked_mul(60),
        "h" => number.checked_mul(3600),
        "d" => number.checked_mul(86_400),
        "w" => number.checked_mul(604_800),
        _ => {
            return Err(OrbitError::InvalidInput(format!(
                "invalid duration unit: {unit} (expected s/m/h/d/w)"
            )));
        }
    };
    seconds.ok_or_else(|| {
        OrbitError::InvalidInput(format!("duration '{raw}' is too large to represent"))
    })
}

/// An optional duration field, parsed with [`parse_duration_seconds`].
pub fn optional_duration_seconds(input: &Value, key: &str) -> Result<Option<u64>, OrbitError> {
    optional_string(input, key)?
        .map(|raw| parse_duration_seconds(&raw))
        .transpose()
}
