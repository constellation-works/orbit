//! Lease tool input: selectors, required fields and lease ids.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::protocol::tool_input::{optional_string_list_alias, required_string};
use serde_json::Value;

use super::super::commands::{canonicalize_file_lock_selectors, parse_file_lock_selectors};

pub(super) const LEASE_ID_PREFIX: &str = "lease-";

pub(super) fn parse_selectors(
    input: &Value,
    keys: &[&str],
    field: &str,
    workspace_root: &Path,
) -> Result<Vec<String>, OrbitError> {
    let raw = optional_string_list_alias(input, keys)?
        .ok_or_else(|| OrbitError::InvalidInput(format!("missing `{field}`")))?;
    canonicalize_file_lock_selectors(
        &parse_file_lock_selectors(raw, field)?,
        field,
        workspace_root,
    )
}

pub(super) fn required_non_empty(input: &Value, field: &str) -> Result<String, OrbitError> {
    let value = required_string(input, &[field], field)?;
    let value = value.trim();
    if value.is_empty() {
        return Err(OrbitError::InvalidInput(format!(
            "`{field}` must not be empty"
        )));
    }
    Ok(value.to_string())
}

pub(super) fn required_lease_id(input: &Value) -> Result<String, OrbitError> {
    let lease_id = required_string(input, &["lease_id", "leaseId", "lease-id"], "lease_id")?;
    validate_lease_id_form(&lease_id)?;
    Ok(lease_id)
}

/// Lease ids are minted as `lease-<id>`; a reservation or task id can never
/// match one, so it is refused rather than answered as "not found".
pub(super) fn validate_lease_id_form(lease_id: &str) -> Result<(), OrbitError> {
    if lease_id.starts_with(LEASE_ID_PREFIX) {
        return Ok(());
    }
    Err(OrbitError::InvalidInput(format!(
        "`lease_id` must have the form `{LEASE_ID_PREFIX}<id>`; got `{lease_id}`"
    )))
}
