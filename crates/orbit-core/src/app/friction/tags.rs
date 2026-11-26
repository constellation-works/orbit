//! `tags`: list the configured taxonomy tags.

use orbit_common::OrbitError;
use serde_json::{Value, json};

use crate::OrbitRuntime;

pub(super) fn tags(runtime: &OrbitRuntime) -> Result<Value, OrbitError> {
    Ok(json!(crate::runtime::friction::store_for(runtime)?.tags()?))
}
