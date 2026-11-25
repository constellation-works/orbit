//! JSON encoding for persisted automation records.

use orbit_common::OrbitError;

pub(in crate::repository) fn encode<T: serde::Serialize>(value: &T) -> Result<String, OrbitError> {
    serde_json::to_string(value).map_err(|e| OrbitError::Store(e.to_string()))
}

pub(in crate::repository) fn decode<T: serde::de::DeserializeOwned>(
    raw: &str,
) -> Result<T, OrbitError> {
    serde_json::from_str(raw)
        .map_err(|e| OrbitError::Store(format!("invalid persisted automation record: {e}")))
}
