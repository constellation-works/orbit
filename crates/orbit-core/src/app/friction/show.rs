//! `show`: show one friction record.

use orbit_common::governance::operation::OperationInput;
use orbit_common::{NotFoundKind, OrbitError};
use serde_json::Value;

use super::record::record_to_json;
use crate::OrbitRuntime;

pub(super) fn show(runtime: &OrbitRuntime, input: &OperationInput) -> Result<Value, OrbitError> {
    show_in(&crate::runtime::friction::store_for(runtime)?, input)
}

fn show_in(
    store: &crate::runtime::friction::WorkspaceFrictions,
    input: &OperationInput,
) -> Result<Value, OrbitError> {
    let id = input.required_string("id")?;
    let Some(stored) = store.show(&id)? else {
        return Err(OrbitError::not_found(NotFoundKind::Friction, id));
    };
    record_to_json(stored)
}
