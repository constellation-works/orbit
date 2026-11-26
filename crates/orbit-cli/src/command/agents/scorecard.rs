use serde_json::Value;

use super::support::{number, selection_line, text};
use crate::output::common::payload::Block;
use crate::output::common::table::{Column, Table};

pub(super) fn blocks(value: &Value) -> Vec<Block> {
    if value["per_task"] == true {
        per_task_blocks(value)
    } else if value["by_type"] == true {
        by_type_blocks(value)
    } else {
        scorecard_blocks(value)
    }
}

/// One row per crew and model, ranked rows first, with a column per term.
///
/// The selection line and the units footer are status, not records, so they
/// are notices: stderr in every mode, which keeps stdout to the rows alone.
fn scorecard_blocks(value: &Value) -> Vec<Block> {
    let terms = value["terms"].as_array().cloned().unwrap_or_default();
    let computed = value["status"] != "not_computed";
    let empty = if computed {
        "no row in the window".to_string()
    } else {
        format!(
            "The {} scorecard is not computed yet.",
            text(&value["role"])
        )
    };
    let mut columns = vec![
        Column::new("RANK").number(),
        Column::new("ROW"),
        Column::new("CREWS"),
        Column::new("UNITS").number(),
        Column::new("SCORE").number(),
        Column::new("INTERVAL"),
    ];
    columns.extend(
        terms
            .iter()
            .map(|term| Column::new(&text(&term["label"]).to_uppercase()).number()),
    );
    let mut table = Table::new(columns)
        .keep_all_columns()
        .empty_message(empty)
        .trailing_notice(selection_line(value));
    for row in value["rows"].as_array().into_iter().flatten() {
        let mut cells = vec![
            if row["unjudged"] == true {
                "unjudged".into()
            } else if row["below_support"] == true {
                "below".into()
            } else {
                number(&row["rank"])
            },
            text(&row["label"]),
            row["crews"]
                .as_array()
                .map(|crews| crews.iter().map(text).collect::<Vec<_>>().join(", "))
                .unwrap_or_else(|| text(&row["crew"])),
            number(&row["units"]),
            number(&row["score"]),
            interval(&row["interval"]),
        ];
        cells.extend(
            terms
                .iter()
                .map(|term| number(&row["counts"][text(&term["key"])])),
        );
        table.add_row(cells);
    }
    if computed {
        let formula = if value["role"] == "implementers" {
            "100 + weighted (actual − expected) per 100 tasks; expectations use the fleet's rates for the same tier mix; tasks tagged no-diff-expected when an attempt was admitted are not implementer work and count nowhere here"
        } else {
            "each term per 100 units, weighted and summed"
        };
        table = table.trailing_notice(format!(
            "Units: {}. Score: {formula}; a row above the fleet average ({}) is pulled down toward it.\nInterval: the 10th to 90th percentile of a fixed-seed resample of the row's own units; `-` for an unranked row.\nWeights: {}",
            text(&value["unit"]),
            number(&value["fleet_average"]),
            terms
                .iter()
                .map(|term| format!("{} × {}", number(&term["weight"]), text(&term["key"])))
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }
    vec![Block::table(table)]
}

/// The by-type matrix: one row per (model, recorded effort, tier, type)
/// cell, with the crews that contributed to it.
fn by_type_blocks(value: &Value) -> Vec<Block> {
    let columns = vec![
        Column::new("CREWS").fixed(),
        Column::new("ROW"),
        Column::new("TIER"),
        Column::new("TYPE"),
        Column::new("N").number(),
        Column::new("RAW").number(),
        Column::new("CREW MEAN").number(),
        Column::new("SCORE").number(),
    ];
    let types = value["types"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .map(|entry| text(&entry["name"]))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    let mut table = Table::new(columns)
        .keep_all_columns()
        .empty_message("no row in the window".to_string())
        .trailing_notice(selection_line(value))
        .trailing_notice(format!(
            "Types: {}; tasks matching none are other. Score: two-level shrinkage toward the crew mean, then toward 100 (K = 30).",
            if types.is_empty() { "none configured".to_string() } else { types },
        ));
    for cell in value["cells"].as_array().into_iter().flatten() {
        table.add_row(vec![
            cell["crews"]
                .as_array()
                .map(|crews| crews.iter().map(text).collect::<Vec<_>>().join(", "))
                .unwrap_or_else(|| text(&cell["crew"])),
            {
                let label = text(&cell["label"]);
                if label.is_empty() {
                    text(&cell["model"])
                } else {
                    label
                }
            },
            text(&cell["tier"]),
            text(&cell["type"]),
            number(&cell["n"]),
            number(&cell["raw"]),
            number(&cell["crew_mean"]),
            number(&cell["score"]),
        ]);
    }
    vec![Block::table(table)]
}

/// The per-task export: one row per implemented task, without naming it.
/// Use `--json` for the machine-readable export the offline report reads.
fn per_task_blocks(value: &Value) -> Vec<Block> {
    let terms = value["terms"].as_array().cloned().unwrap_or_default();
    let mut columns = vec![
        Column::new("CREWS").fixed(),
        Column::new("ROW"),
        Column::new("TIER"),
        Column::new("TYPE"),
        Column::new("TAGS"),
        Column::new("PATHS"),
    ];
    columns.extend(
        terms
            .iter()
            .map(|term| Column::new(&text(&term["label"]).to_uppercase()).number()),
    );
    let mut table = Table::new(columns)
        .keep_all_columns()
        .empty_message("no implemented task in the window".to_string())
        .trailing_notice(selection_line(value))
        .trailing_notice(
            "One row per implemented task; use --json for the machine-readable export.".to_string(),
        );
    for task in value["tasks"].as_array().into_iter().flatten() {
        let join = |key: &str| {
            task[key]
                .as_array()
                .map(|items| items.iter().map(text).collect::<Vec<_>>().join(","))
                .unwrap_or_default()
        };
        let mut cells = vec![
            task["crews"]
                .as_array()
                .map(|crews| crews.iter().map(text).collect::<Vec<_>>().join(", "))
                .unwrap_or_else(|| text(&task["crew"])),
            {
                let label = text(&task["label"]);
                if label.is_empty() {
                    text(&task["model"])
                } else {
                    label
                }
            },
            text(&task["tier"]),
            text(&task["type"]),
            join("tags"),
            join("path_prefixes"),
        ];
        cells.extend(
            terms
                .iter()
                .map(|term| number(&task["counts"][text(&term["key"])])),
        );
        table.add_row(cells);
    }
    vec![Block::table(table)]
}

/// A row's score interval as `p10 to p90`; `-` when the row has none.
fn interval(value: &Value) -> String {
    if value["p10"].is_number() && value["p90"].is_number() {
        format!("{} to {}", number(&value["p10"]), number(&value["p90"]))
    } else {
        "-".into()
    }
}
