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

pub fn strip_retired_task_add_input_fields(input: &mut Value) -> Vec<&'static str> {
    let Some(object) = input.as_object_mut() else {
        return Vec::new();
    };

    let mut ignored = Vec::new();
    for field in RETIRED_TASK_ADD_INPUT_FIELDS {
        if object.remove(*field).is_some() {
            ignored.push(*field);
        }
    }
    ignored
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
