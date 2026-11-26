use serde_json::Value;

use super::support::text;
use crate::output::common::payload::Block;

pub(super) fn block(value: &Value) -> Block {
    Block::text(format!(
        "Checkpoint recorded for cycle {}.",
        text(&value["checkpoint"]["cycle"])
    ))
}
