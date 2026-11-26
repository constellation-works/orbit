//! Pipeline and workflow input fields accepted by the tool entry.

use orbit_common::OrbitError;
use serde_json::Value;

pub(super) fn require_object_field<'a>(
    input: &'a Value,
    field: &str,
) -> Result<&'a Value, OrbitError> {
    let value = input
        .get(field)
        .ok_or_else(|| OrbitError::InvalidInput(format!("missing `{field}`")))?;
    if !value.is_object() {
        return Err(OrbitError::InvalidInput(format!(
            "`{field}` must be a JSON object"
        )));
    }
    Ok(value)
}

pub(super) fn parse_string_array_field(
    input: &Value,
    field: &str,
) -> Result<Vec<String>, OrbitError> {
    let value = input
        .get(field)
        .ok_or_else(|| OrbitError::InvalidInput(format!("missing `{field}`")))?;
    let values = match value {
        Value::String(raw) => vec![raw.as_str()],
        Value::Array(items) => {
            if items.is_empty() {
                return Err(OrbitError::InvalidInput(format!(
                    "`{field}` must contain at least one value"
                )));
            }
            items
                .iter()
                .map(|item| {
                    item.as_str().ok_or_else(|| {
                        OrbitError::InvalidInput(format!("`{field}` entries must be strings"))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?
        }
        _ => {
            return Err(OrbitError::InvalidInput(format!(
                "`{field}` must be a string or array of strings"
            )));
        }
    };
    values
        .into_iter()
        .map(|raw| {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Err(OrbitError::InvalidInput(format!(
                    "`{field}` entries must not be empty"
                )));
            }
            Ok(trimmed.to_string())
        })
        .collect()
}

pub(super) fn parse_optional_string_array_field(
    input: &Value,
    field: &str,
) -> Result<Vec<String>, OrbitError> {
    match input.get(field) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) if items.is_empty() => Ok(Vec::new()),
        Some(_) => parse_string_array_field(input, field),
    }
}

pub(super) fn parse_optional_poll_interval_seconds(
    input: &Value,
) -> Result<Option<u64>, OrbitError> {
    let Some(value) = input.get("poll_interval_seconds") else {
        return Ok(None);
    };
    let seconds = match value {
        Value::Number(number) => number.as_f64(),
        Value::String(raw) => raw.trim().parse::<f64>().ok(),
        _ => None,
    }
    .ok_or_else(|| {
        OrbitError::InvalidInput("`poll_interval_seconds` must be a positive number".to_string())
    })?;
    if !seconds.is_finite() || seconds < 0.0 {
        return Err(OrbitError::InvalidInput(
            "`poll_interval_seconds` must be a positive number".to_string(),
        ));
    }
    Ok(Some(seconds.floor().max(1.0) as u64))
}

pub(super) fn parse_optional_timeout_seconds(input: &Value) -> Result<Option<u64>, OrbitError> {
    let Some(value) = input.get("timeout_seconds") else {
        return Ok(None);
    };
    let seconds = match value {
        Value::Number(number) => number.as_u64(),
        Value::String(raw) => raw.trim().parse::<u64>().ok(),
        _ => None,
    }
    .ok_or_else(|| {
        OrbitError::InvalidInput("`timeout_seconds` must be an unsigned integer".to_string())
    })?;
    Ok(Some(seconds))
}
