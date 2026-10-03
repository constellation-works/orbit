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

pub fn definition_epoch<T: serde::Serialize>(definition: &T) -> Result<String, AutomationError> {
    serde_json::to_vec(definition)
        .map(|bytes| digest(&bytes))
        .map_err(|e| AutomationError::Evidence(e.to_string()))
}
