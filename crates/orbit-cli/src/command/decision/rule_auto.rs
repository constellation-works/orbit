use orbit_core::app::task::Outcome;
use serde_json::Value;

use crate::command::{CommandOut, Payload};
use crate::output::common::payload::Block;

pub(super) fn render(outcome: Outcome) -> CommandOut {
    let value = outcome.into_answer()?;
    let word = if value["auto"] == Value::Bool(true) {
        "on"
    } else {
        "off"
    };
    let blocks = vec![Block::text(format!(
        "Set auto {word} on `{}`.",
        value["rule"].as_str().unwrap_or_default()
    ))];
    Ok(Payload::blocks(value, blocks).into())
}
