use orbit_core::app::task::Outcome;
use serde_json::Value;

use crate::command::{CommandOut, Payload};
use crate::output::common::payload::Block;

pub(super) fn render(outcome: Outcome) -> CommandOut {
    let value = outcome.into_answer()?;
    let count = value.get("count").and_then(Value::as_u64).unwrap_or(0);
    let paths = value
        .get("proposals")
        .and_then(Value::as_array)
        .map(|proposals| {
            proposals
                .iter()
                .filter_map(|proposal| proposal.get("path").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    let blocks = vec![Block::text(format!(
        "Proposed {count} corpus file(s). Nothing was written. Open a pull request with these proposals; do not edit a rule outside that pull request.\n{paths}"
    ))];
    Ok(Payload::blocks(value, blocks).into())
}
