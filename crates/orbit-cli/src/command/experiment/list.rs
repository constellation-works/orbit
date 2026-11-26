use serde_json::Value;

use super::support::text;
use crate::output::common::payload::Block;
use crate::output::common::table::{Column, Table};

pub(super) fn blocks(value: &Value) -> Vec<Block> {
    vec![Block::table(list_table(value))]
}

fn list_table(value: &Value) -> Table {
    let mut table = Table::new(vec![
        Column::new("ID").fixed(),
        Column::new("NAME"),
        Column::new("STATUS"),
        Column::new("MODE"),
        Column::new("SEGMENT").number(),
        Column::new("ARMS"),
        Column::new("UPDATED"),
    ])
    .empty_message("no experiments");
    for experiment in value["experiments"].as_array().into_iter().flatten() {
        table.add_row(vec![
            text(&experiment["id"]),
            text(&experiment["name"]),
            text(&experiment["status"]),
            text(&experiment["mode"]),
            text(&experiment["segment"]),
            arm_summary(&experiment["arms"]),
            text(&experiment["updated_at"]),
        ]);
    }
    table
}

/// `name:weight` per active arm, the control marked `*`.
fn arm_summary(arms: &Value) -> String {
    arms.as_array()
        .into_iter()
        .flatten()
        .filter(|arm| arm["retired_segment"].is_null())
        .map(|arm| {
            let marker = if arm["control"] == Value::Bool(true) {
                "*"
            } else {
                ""
            };
            format!("{}{marker}:{}", text(&arm["name"]), text(&arm["weight"]))
        })
        .collect::<Vec<_>>()
        .join(" ")
}
