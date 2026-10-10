use orbit_common::OrbitError;
use serde_json::Value;

use crate::executor::automation::input::input_string_field;

pub(super) fn refused(message: impl Into<String>) -> OrbitError {
    OrbitError::PolicyDenied(message.into())
}

pub(super) fn required_workspace(input: &Value) -> Result<std::path::PathBuf, OrbitError> {
    input_string_field(input, "workspace_path")
        .map(std::path::PathBuf::from)
        .ok_or_else(|| OrbitError::InvalidInput("workspace_path is required".to_string()))
}
