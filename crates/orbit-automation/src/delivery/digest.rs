//! Content digests that identify frozen batches and definitions.

use crate::AutomationError;
use orbit_types::workflow::automation::CoverageBatch;
use sha2::{Digest, Sha256};

pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn input_digest(batch: &CoverageBatch) -> Result<String, AutomationError> {
    serde_json::to_vec(batch)
        .map(|bytes| digest(&bytes))
        .map_err(|e| AutomationError::Evidence(e.to_string()))
}

/// Hash typed definitions in their existing serde field order. JSON-valued
/// identities must use `json_definition_epoch` instead.
pub fn definition_epoch<T: serde::Serialize>(definition: &T) -> Result<String, AutomationError> {
    serde_json::to_vec(definition)
        .map(|bytes| digest(&bytes))
        .map_err(|e| AutomationError::Evidence(e.to_string()))
}

/// Preserve the sorted JSON object bytes used before `preserve_order` was
/// enabled for plugin panels. Arrays remain ordered; typed struct hashes must
/// not pass through this conversion because their fields were never sorted.
pub(crate) fn json_definition_epoch(
    mut value: serde_json::Value,
) -> Result<String, AutomationError> {
    value.sort_all_objects();
    definition_epoch(&value)
}
