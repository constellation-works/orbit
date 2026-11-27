use orbit_common::OrbitError;
use serde_json::Value;

pub(super) fn serialize<T: serde::Serialize>(value: &T) -> Result<Value, OrbitError> {
    serde_json::to_value(value)
        .map_err(|error| OrbitError::Execution(format!("serialize decision channels: {error}")))
}

pub(super) fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_string()
}
