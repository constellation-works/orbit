//! `resolve`: mark a record resolved.

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::governance::operation::OperationInput;
use serde_json::Value;

use super::record::record_to_json;
use crate::OrbitRuntime;

pub(super) fn resolve(runtime: &OrbitRuntime, input: &OperationInput) -> Result<Value, OrbitError> {
    let id = input.required_string("id")?;
    runtime.ensure_local_friction_resolution_permitted(&id)?;
    let stored = crate::runtime::stored::friction::store_for(runtime)?.resolve(&id, Utc::now())?;
    record_to_json(stored)
}
