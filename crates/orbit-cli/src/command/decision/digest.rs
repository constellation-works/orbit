use orbit_core::app::task::Outcome;
use serde_json::Value;

use super::list::list_blocks;
use crate::command::{CommandOut, Payload};
use crate::output::common::payload::Block;

pub(super) fn render(outcome: Outcome) -> CommandOut {
    let value = outcome.into_answer()?;
    let rendered = digest_blocks(&value);
    Ok(Payload::blocks(value, rendered).into())
}

fn digest_blocks(value: &Value) -> Vec<Block> {
    let raised = value["raised"].as_array().map_or(0, Vec::len);
    let expiring = value["expiring"].as_array().map_or(0, Vec::len);
    let mut blocks = vec![Block::text(format!(
        "Digest: {raised} request(s) raised, {expiring} open request(s) expiring within 24 hours."
    ))];
    blocks.extend(list_blocks(value));
    blocks
}
