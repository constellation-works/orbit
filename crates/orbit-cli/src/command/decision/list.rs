use orbit_core::app::task::Outcome;
use serde_json::Value;

use super::support::text;
use crate::command::{CommandOut, Payload};
use crate::output::common::payload::Block;
use crate::output::common::table::{Column, Table};

pub(super) fn render(outcome: Outcome) -> CommandOut {
    let value = outcome.into_answer()?;
    let rendered = list_blocks(&value);
    Ok(Payload::blocks(value, rendered).into())
}

pub(super) fn list_blocks(value: &Value) -> Vec<Block> {
    let mut table = Table::new(vec![
        Column::new("REQUEST").fixed(),
        Column::new("TASK").fixed(),
        Column::new("GATE").fixed(),
        Column::new("STATE").fixed(),
        Column::new("RISK").fixed(),
        Column::new("CATEGORY"),
        Column::new("ANSWERS"),
        Column::new("EXPIRES").fixed(),
    ])
    .empty_message("no decision requests");
    for request in value["requests"].as_array().into_iter().flatten() {
        let answers = request["answers"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|answer| answer["id"].as_str())
            .collect::<Vec<_>>()
            .join(", ");
        table.add_row(vec![
            text(&request["id"]),
            text(&request["task_id"]),
            text(&request["gate"]),
            text(&request["state"]),
            text(&request["risk_tier"]),
            text(&request["category"]),
            answers,
            text(&request["expires_at"]),
        ]);
    }
    let mut blocks = vec![Block::table(table)];
    for warning in value["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        blocks.push(Block::text(format!("warning: {warning}")));
    }
    blocks
}
