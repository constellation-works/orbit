use serde_json::Value;

use super::support::display;
use crate::output::common::payload::Block;

pub(super) fn block(value: &Value) -> Block {
    Block::text(format!(
        "{} [{}]\n{}\nOccurrences: {}; minutes: {}; slot minutes: {}",
        display(&value["signature"]),
        display(&value["state"]),
        display(&value["title"]),
        display(&value["occurrence_count"]),
        display(&value["minutes"]),
        display(&value["slot_minutes"])
    ))
}
