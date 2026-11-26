use serde_json::Value;

use super::support::display;
use crate::output::common::payload::Block;
use crate::output::common::table::{Column, Table};

pub(super) fn block(value: &Value) -> Block {
    let mut table = Table::new(vec![
        Column::new("ID").fixed(),
        Column::new("SIGNATURE"),
        Column::new("STATE"),
        Column::new("OCCURRENCES"),
        Column::new("MANDATES"),
        Column::new("MINUTES"),
        Column::new("SLOT MINUTES"),
    ])
    .empty_message("no evolution records");
    for row in value["records"].as_array().into_iter().flatten() {
        table.add_row(
            [
                "id",
                "signature",
                "state",
                "occurrence_count",
                "mandate_count",
                "minutes",
                "slot_minutes",
            ]
            .iter()
            .map(|key| display(&row[*key]))
            .collect(),
        );
    }
    Block::table(table)
}
