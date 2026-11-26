use serde_json::Value;

use super::support::display;
use crate::output::common::payload::Block;

pub(super) fn block(value: &Value) -> Block {
    Block::text(if value["record"].is_null() {
        "Cost recorded.".into()
    } else {
        format!(
            "Cost recorded for {} ({}).",
            display(&value["record"]["signature"]),
            display(&value["record"]["id"])
        )
    })
}
