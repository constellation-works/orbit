use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::fs::io::{atomic_write_text, atomic_write_text_volatile};

pub(crate) fn parse_yaml_with<T: serde::de::DeserializeOwned, F>(
    raw: &str,
    path: &Path,
    invalid: F,
) -> Result<T, OrbitError>
where
    F: FnOnce(&Path, serde_yaml::Error) -> OrbitError,
{
    serde_yaml::from_str(raw).map_err(|err| invalid(path, err))
}

pub(crate) fn serialize_yaml_with<T: serde::Serialize, F>(
    value: &T,
    invalid: F,
) -> Result<String, OrbitError>
where
    F: FnOnce(serde_yaml::Error) -> OrbitError,
{
    serde_yaml::to_string(value).map_err(invalid)
}

pub(crate) fn write_yaml_atomic_with<T: serde::Serialize, F>(
    path: &Path,
    value: &T,
    invalid: F,
) -> Result<(), OrbitError>
where
    F: FnOnce(serde_yaml::Error) -> OrbitError,
{
    let yaml = serialize_yaml_with(value, invalid)?;
    atomic_write_text_volatile(path, &yaml).map_err(|err| OrbitError::from_write_io(path, err))
}

/// Durable YAML publish: temp-file `sync_all` plus parent-directory fsync.
/// Used for task envelopes, artifact manifests, and the pending-write record.
pub(crate) fn write_yaml_durable_with<T: serde::Serialize, F>(
    path: &Path,
    value: &T,
    invalid: F,
) -> Result<(), OrbitError>
where
    F: FnOnce(serde_yaml::Error) -> OrbitError,
{
    let yaml = serialize_yaml_with(value, invalid)?;
    atomic_write_text(path, &yaml).map_err(|err| OrbitError::from_write_io(path, err))
}
