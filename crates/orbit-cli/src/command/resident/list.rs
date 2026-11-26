use serde_json::Value;

use super::support::{joined, text};
use crate::output::common::payload::Block;
use crate::output::common::table::{Column, Table};

pub(super) fn block(value: &Value) -> Block {
    Block::table(list_table(value))
}

fn list_table(value: &Value) -> Table {
    let mut table = Table::new(vec![
        Column::new("TASK").fixed(),
        Column::new("STATE"),
        Column::new("RIGHTS"),
        Column::new("WORKSPACES"),
        Column::new("REQUESTS"),
        Column::new("GOAL"),
    ])
    .empty_message("no resident mandates");
    for row in value["mandates"].as_array().into_iter().flatten() {
        table.add_row(vec![
            text(&row["task_id"]),
            if row["stalled"] == true {
                format!("{} (stalled)", text(&row["state"]))
            } else {
                text(&row["state"])
            },
            joined(&row["rights"]),
            joined(&row["workspaces"]),
            text(&row["open_requests"]),
            text(&row["goal"]),
        ]);
    }
    table
}
