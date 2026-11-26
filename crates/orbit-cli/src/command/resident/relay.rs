use serde_json::Value;

use super::support::text;
use crate::output::common::payload::Block;

pub(super) fn block(value: &Value) -> Block {
    Block::text(format!(
        "Relayed to {}: a resident.answer comment wakes the resident.",
        text(&value["task_id"])
    ))
}
