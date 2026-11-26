use serde_json::Value;

use super::support::{notice_detail, text};
use crate::output::common::payload::Block;

pub(super) fn block(value: &Value) -> Block {
    let verb = if value["already_inactive"].as_bool() == Some(true) {
        "already inactive"
    } else {
        "closed"
    };
    Block::text(format!(
        "{verb} {}\n{}",
        text(&value["notice"]["id"]),
        notice_detail(&value["notice"]),
    ))
}
