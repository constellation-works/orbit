use serde_json::Value;

use super::support::{number, text};
use crate::output::common::payload::Block;
use crate::output::common::table::{Column, Table};

/// One row per recorded required-check failure, newest first, with the
/// per-class counts as a notice.
pub(super) fn blocks(value: &Value) -> Vec<Block> {
    let columns = vec![
        Column::new("RECORDED"),
        Column::new("TASK").fixed(),
        Column::new("CLASS"),
        Column::new("HEAD").fixed(),
        Column::new("CHECK"),
        Column::new("SIGNATURE"),
    ];
    let mut table = Table::new(columns).empty_message("no recorded CI failure in the window");
    for record in value["records"].as_array().into_iter().flatten() {
        let class = text(&record["class"]);
        let recorded = text(&record["recorded_class"]);
        table.add_row(vec![
            text(&record["recorded_at"]),
            text(&record["task_id"]),
            if class == recorded {
                class
            } else {
                format!("{class} (was {recorded})")
            },
            text(&record["head_sha"]).chars().take(12).collect(),
            text(&record["check"]),
            text(&record["signature"]),
        ]);
    }
    let counts = ["code", "flaky", "infrastructure", "unknown"]
        .iter()
        .map(|class| format!("{class} {}", number(&value["counts"][*class])))
        .collect::<Vec<_>>()
        .join(", ");
    table = table.trailing_notice(format!("Window: {}. {counts}.", text(&value["window"])));
    vec![Block::table(table)]
}
