use std::fmt::Write as _;

use serde_json::Value;

use super::support::{number, selection_line, text};
use crate::output::common::payload::Block;
use crate::output::common::table::{Column, Table};

/// One row's terms, then the tasks behind each nonzero count.
pub(super) fn blocks(value: &Value) -> Vec<Block> {
    let row = &value["row"];
    let standing = if row["unjudged"] == true {
        "unjudged: no unit judged yet".to_string()
    } else if row["below_support"] == true {
        "below minimum support".to_string()
    } else {
        format!("rank {}", number(&row["rank"]))
    };
    let mut blocks = vec![Block::text(format!(
        "{} ({standing}) · crews {}\n{}\n{} {} · score {} · fleet average {}",
        text(&row["label"]),
        row["crews"]
            .as_array()
            .map(|crews| crews.iter().map(text).collect::<Vec<_>>().join(", "))
            .unwrap_or_else(|| text(&row["crew"])),
        selection_line(value),
        number(&row["units"]),
        text(&value["unit"]),
        number(&row["score"]),
        number(&value["fleet_average"]),
    ))];

    blocks.push(Block::text(format!(
        "Completed invocations by crew and effort, not this row's scored units: {}",
        effort_labels(&row["effort_breakdown"])
    )));
    if let Some(sources) = effort_source_labels(&row["effort_sources"]) {
        blocks.push(Block::text(format!("Effort source: {sources}")));
    }

    let terms = value["terms"].as_array().cloned().unwrap_or_default();
    let normalised = row["expected"].is_object();
    if normalised {
        blocks.push(Block::text(format!("Raw score starts at {}; contributions compare actual with expected counts for the same tier mix.", number(&row["score_base"]))));
    }
    let mut columns = vec![
        Column::new("TERM").fixed(),
        Column::new("WEIGHT").number(),
        Column::new("COUNT").number(),
        Column::new("PER 100").number(),
        Column::new("CONTRIBUTION").number(),
    ];
    if normalised {
        columns.insert(3, Column::new("EXPECTED").number());
    }
    let mut table = Table::new(columns).keep_all_columns();
    for term in &terms {
        let mut cells = vec![
            text(&term["label"]),
            number(&term["weight"]),
            number(&term["count"]),
            number(&term["per_100"]),
            number(&term["contribution"]),
        ];
        if normalised {
            cells.insert(3, number(&row["expected"][text(&term["key"])]));
        }
        table.add_row(cells);
    }
    blocks.push(Block::table(table));

    let mut out = String::new();
    for term in &terms {
        let tasks = term["tasks"].as_array().cloned().unwrap_or_default();
        if tasks.is_empty() {
            continue;
        }
        let _ = writeln!(
            out,
            "\n{} ({})",
            text(&term["label"]),
            number(&term["count"])
        );
        for task in &tasks {
            let _ = writeln!(
                out,
                "  {}  {}  {}",
                text(&task["task_id"]),
                number(&task["count"]),
                text(&task["title"])
            );
            for occurrence in task["occurrences"].as_array().into_iter().flatten() {
                let at = text(&occurrence["at"]);
                let _ = writeln!(
                    out,
                    "    {}  {}",
                    at.get(..16).unwrap_or(&at),
                    text(&occurrence["detail"])
                );
            }
        }
    }
    if !out.is_empty() {
        blocks.push(Block::text(out));
    }
    blocks
}

/// The row's units by the source that decided their effort, as
/// `invocation 12, crew label 3`; `None` when the row reports none.
fn effort_source_labels(value: &Value) -> Option<String> {
    let sources = value.as_object().filter(|sources| !sources.is_empty())?;
    Some(
        sources
            .iter()
            .map(|(source, units)| format!("{} {}", source.replace('_', " "), number(units)))
            .collect::<Vec<_>>()
            .join(", "),
    )
}

/// A breakdown entry's effort in the same words as the dashboard: a null
/// effort is the provider default, and `"unknown"` (or a missing name) is
/// effort unknown. The two stay distinct.
fn recorded_effort_name(value: &Value) -> String {
    if value.is_null() {
        return "provider default".to_string();
    }
    let name = text(value);
    if name.is_empty() || name == "unknown" {
        "effort unknown".to_string()
    } else {
        name
    }
}

/// Completed invocations by contributing crew and recorded effort. This is
/// not the row's scored units, so each entry names its crew.
fn effort_labels(value: &Value) -> String {
    let labels = value
        .as_array()
        .into_iter()
        .flatten()
        .map(|row| {
            let effort = recorded_effort_name(&row["reasoning_effort"]);
            let crew = text(&row["crew"]);
            let who = if crew.is_empty() {
                effort
            } else {
                format!("{crew} / {effort}")
            };
            if row.get("invocations").is_some_and(|count| !count.is_null()) {
                format!(
                    "{who} ({} invocations, {} tasks)",
                    number(&row["invocations"]),
                    number(&row["tasks"])
                )
            } else {
                who
            }
        })
        .collect::<Vec<_>>();
    if labels.is_empty() {
        "effort unknown".into()
    } else {
        labels.join("; ")
    }
}
