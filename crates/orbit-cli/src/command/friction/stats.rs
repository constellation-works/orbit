use serde_json::Value;

use crate::command::{CommandOut, Payload};

// Aggregate rates have no useful flat rendering.
pub(super) fn render(value: &Value) -> CommandOut {
    Ok(Payload::document(value.clone()).into())
}
