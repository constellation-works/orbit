//! JSON encoding for persisted automation records.

use orbit_common::OrbitError;

pub(super) fn encode<T: serde::Serialize>(value: &T) -> Result<String, OrbitError> {
    serde_json::to_string(value).map_err(|e| OrbitError::Store(e.to_string()))
}

pub(super) fn decode<T: serde::de::DeserializeOwned>(raw: &str) -> Result<T, OrbitError> {
    serde_json::from_str(raw)
        .map_err(|e| OrbitError::Store(format!("invalid persisted automation record: {e}")))
}

/// Version the persisted consumer envelope at the existing epoch field. Older
/// readers require a string there and therefore refuse the checkpoint before
/// constructing a lossy snapshot. Public diagnostic JSON keeps its string epoch.
pub(super) fn encode_state(
    state: &orbit_types::workflow::automation::AutomationState,
) -> Result<String, OrbitError> {
    let mut value = serde_json::to_value(state).map_err(|e| OrbitError::Store(e.to_string()))?;
    if state.independent_review {
        value["epoch"] = serde_json::json!({"version": 2, "identity": state.epoch});
    }
    encode(&value)
}

pub(super) fn decode_state(
    raw: &str,
) -> Result<orbit_types::workflow::automation::AutomationState, OrbitError> {
    let mut value: serde_json::Value = decode(raw)?;
    if value.get("epoch").is_some_and(serde_json::Value::is_object) {
        let epoch = &value["epoch"];
        if epoch.get("version").and_then(serde_json::Value::as_u64) != Some(2)
            || epoch
                .get("identity")
                .and_then(serde_json::Value::as_str)
                .is_none()
            || value
                .get("independent_review")
                .and_then(serde_json::Value::as_bool)
                != Some(true)
        {
            return Err(OrbitError::Store(
                "unsupported automation checkpoint format".into(),
            ));
        }
        value["epoch"] = epoch["identity"].clone();
    } else if value
        .get("independent_review")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
        || value
            .get("reservations")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|items| !items.is_empty())
    {
        return Err(OrbitError::Store(
            "unversioned independent automation checkpoint".into(),
        ));
    }
    serde_json::from_value(value)
        .map_err(|e| OrbitError::Store(format!("invalid persisted automation record: {e}")))
}
