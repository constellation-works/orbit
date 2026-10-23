use orbit_common::OrbitError;
use serde_json::Value;

pub(super) fn required_string<'a>(input: &'a Value, key: &str) -> Result<&'a str, OrbitError> {
    optional_string(input, key).ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "private automation VCS operation requires non-empty '{key}' metadata"
        ))
    })
}

pub(super) fn optional_string<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

pub(super) fn reject_option_like(label: &str, value: &str) -> Result<(), OrbitError> {
    if value.starts_with('-') {
        return Err(OrbitError::InvalidInput(format!(
            "private automation VCS {label} must not start with '-'"
        )));
    }
    Ok(())
}

pub(super) fn valid_expected_remote_sha(value: Option<&str>) -> bool {
    value.is_some_and(|sha| {
        matches!(sha.len(), 40 | 64) && sha.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

/// Read a provider string without changing its contents or refusal context.
pub(super) fn text<'a>(
    value: &'a Value,
    key: &str,
    refuse: fn(&str) -> OrbitError,
) -> Result<&'a str, OrbitError> {
    value[key]
        .as_str()
        .filter(|text| !text.is_empty())
        .ok_or_else(|| refuse(key))
}

/// Provider identities must be positive unsigned integers.
pub(super) fn number(
    value: &Value,
    key: &str,
    refuse: fn(&str) -> OrbitError,
) -> Result<u64, OrbitError> {
    value[key]
        .as_u64()
        .filter(|number| *number > 0)
        .ok_or_else(|| refuse(key))
}

/// Refuse a provider identity that differs from the expected observation.
pub(super) fn same(
    actual: &Value,
    expected: &Value,
    label: &str,
    refuse: fn(&str) -> OrbitError,
) -> Result<(), OrbitError> {
    if actual != expected {
        return Err(refuse(&format!("foreign or changed {label}")));
    }
    Ok(())
}
