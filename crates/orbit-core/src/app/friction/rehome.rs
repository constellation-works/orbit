//! `rehome`: move a record into the workspace that owns it.

use orbit_common::OrbitError;
use orbit_common::governance::operation::OperationInput;
use serde_json::Value;

use super::record::rehome_to_json;
use crate::OrbitRuntime;

pub(super) fn rehome(runtime: &OrbitRuntime, input: &OperationInput) -> Result<Value, OrbitError> {
    let id = input.required_string("id")?;
    let to_workspace = input.required_string("to_workspace")?;
    rehome_to_json(
        runtime.rehome_friction(&id, &to_workspace, None)?,
        Vec::new(),
    )
}
