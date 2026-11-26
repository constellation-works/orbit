use chrono::{DateTime, Utc};
use serde_json::Value;

use super::support::{span, text, timestamp};
use crate::output::common::payload::Block;
use crate::output::common::table::{Column, Table};

pub(super) fn block(value: &Value, now: DateTime<Utc>) -> Block {
    Block::table(list_table(value, now))
}

fn list_table(value: &Value, now: DateTime<Utc>) -> Table {
    let mut table = Table::new(vec![
        Column::new("ID").fixed(),
        Column::new("KIND"),
        Column::new("SCOPE"),
        Column::new("TITLE"),
        Column::new("AUTHOR"),
        Column::new("AGE"),
        Column::new("EXPIRES"),
    ])
    .empty_message("no notices");
    for notice in value["notices"].as_array().into_iter().flatten() {
        let age = timestamp(&notice["opened_at"])
            .map(|at| span((now - at).num_seconds()))
            .unwrap_or_default();
        let expires = match notice["status"].as_str() {
            Some("open") => timestamp(&notice["expires_at"])
                .map(|at| format!("in {}", span((at - now).num_seconds())))
                .unwrap_or_default(),
            other => other.unwrap_or_default().to_string(),
        };
        table.add_row(vec![
            text(&notice["id"]),
            text(&notice["kind"]),
            text(&notice["scope"]),
            text(&notice["title"]),
            text(&notice["author"]["class"]),
            age,
            expires,
        ]);
    }
    if let Some(degraded) = value["degraded"]
        .as_array()
        .filter(|homes| !homes.is_empty())
    {
        let homes: Vec<String> = degraded.iter().map(text).collect();
        table = table.trailing_notice(format!("not read: {}", homes.join(", ")));
    }
    table
}
