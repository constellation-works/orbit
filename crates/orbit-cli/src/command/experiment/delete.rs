use serde_json::Value;

use super::support::text;
use crate::output::common::payload::Block;

pub(super) fn blocks(value: &Value) -> Vec<Block> {
    vec![Block::text(format!(
        "deleted {} {} ({}, {})",
        text(&value["deleted"]),
        text(&value["name"]),
        text(&value["status"]),
        text(&value["mode"]),
    ))]
}
