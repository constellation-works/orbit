//! `orbit task eligible` rendering.

use comfy_table::Cell;
use serde_json::{Map, Value};

use crate::command::{Block, CommandOut, Payload};
use crate::output::color::Domain;
use crate::output::table::{Column, Table};

pub(super) fn render(input: &Map<String, Value>, value: Value) -> CommandOut {
    let explain = input.get("explain") == Some(&Value::Bool(true));
    let blocks = eligible_blocks(&value, explain);
    Ok(Payload::blocks(value, blocks).into())
}

fn eligible_blocks(doc: &Value, explain: bool) -> Vec<Block> {
    let tasks = doc["tasks"].as_array().map_or(&[][..], Vec::as_slice);
    let mut table = Table::new(vec![
        Column::new("ID").fixed(),
        Column::new("STATUS").fixed(),
        Column::new("PRIORITY").fixed(),
        Column::new("COMPLEXITY").fixed(),
        Column::new("TITLE"),
    ])
    .empty_message("no eligible tasks");
    for task in tasks {
        table.add_row(vec![
            Cell::new(text(&task["id"])),
            crate::output::color::cell(text(&task["status"]), Domain::TaskStatus),
            crate::output::color::cell(text(&task["priority"]), Domain::Priority),
            Cell::new(task["complexity"].as_str().unwrap_or("-")),
            Cell::new(text(&task["title"])),
        ]);
    }
    if doc["truncated"].as_bool() == Some(true) {
        table = table.trailing_notice(format!(
            "showing {} of {} eligible tasks; use --limit N to see more",
            tasks.len(),
            doc["total"].as_u64().unwrap_or_default()
        ));
    }
    let mut blocks = vec![Block::table(table)];
    if !explain {
        return blocks;
    }

    let conflicting = doc["conflicting"].as_array().map_or(&[][..], Vec::as_slice);
    if conflicting.is_empty() {
        blocks.push(Block::text("No candidate conflicts with in-flight work."));
        return blocks;
    }
    blocks.push(Block::text("Held back by in-progress or review work:"));
    let mut held = Table::new(vec![
        Column::new("ID").fixed(),
        Column::new("STATUS").fixed(),
        Column::new("FILE").path(),
        Column::new("HELD BY").fixed(),
    ])
    .keep_all_columns();
    for task in conflicting {
        for conflict in task["conflicts"].as_array().map_or(&[][..], Vec::as_slice) {
            held.add_row(vec![
                Cell::new(text(&task["id"])),
                crate::output::color::cell(text(&task["status"]), Domain::TaskStatus),
                Cell::new(text(&conflict["requested_file"])),
                Cell::new(text(&conflict["locking_task_id"])),
            ]);
        }
    }
    blocks.push(Block::table(held));
    blocks
}

fn text(value: &Value) -> &str {
    value.as_str().unwrap_or_default()
}
