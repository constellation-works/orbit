use serde_json::Value;

use crate::output::common::payload::Block;

pub(super) fn blocks(value: &Value) -> Vec<Block> {
    vec![Block::text(value.to_string())]
}
