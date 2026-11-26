use serde_json::Value;

use super::support::display;
use crate::output::common::payload::Block;

pub(super) fn block(value: &Value) -> Block {
    Block::text(format!(
        "{} suppressed until {}.",
        display(&value["signature"]),
        display(&value["declined_until"])
    ))
}
