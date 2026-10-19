//! The orchestrator scorecard CLI, derived from the orchestrator operation
//! registry.
//!
//! Both verbs call the read-only `orbit.orchestrator.*` tools, so `--json` is
//! the payload the dashboard's Orchestrators view reads. What is left here is
//! the text rendering: one row per orchestrator for `scorecard`, and one
//! orchestrator's components with the tasks behind every count for `show`.

use std::fmt::Write as _;

use clap::Args;
use orbit_common::governance::orchestrator::OrchestratorVerb;
use orbit_core::OrbitRuntime;
use serde_json::Value;

use super::operation_args::Invocation;
use crate::command::{CommandOut, Execute, Payload};
use crate::output::payload::Block;
use crate::output::table::{Column, Table};

/// The scorecard columns the text view shows, components before the
/// composite; `--json` carries every component.
const TEXT_COLUMNS: &[(&str, &str)] = &[
    ("owned", "OWNED"),
    ("landed", "LANDED"),
    ("weighted_landed", "WEIGHTED"),
    ("net_initiative", "NET INIT"),
    ("unknown_origin", "NO ORIGIN"),
    ("spec_defects", "SPEC DEF"),
    ("rework", "REWORK"),
];

#[derive(Args)]
#[command(about = orbit_common::governance::orchestrator::ORCHESTRATOR.cli_about)]
pub struct OrchestratorCommand {
    #[command(subcommand)]
    pub command: Invocation<OrchestratorVerb>,
}

impl Execute for OrchestratorCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let Invocation { spec, input } = self.command;
        let value = runtime.run_tool(spec.tool_name, Value::Object(input))?;
        let blocks = match spec.verb {
            OrchestratorVerb::Scorecard => scorecard_blocks(&value),
            OrchestratorVerb::Show => show_blocks(&value),
        };
        Ok(Payload::blocks(value, blocks).into())
    }
}

fn scorecard_blocks(value: &Value) -> Vec<Block> {
    let mut columns = vec![Column::new("ORCHESTRATOR").fixed()];
    columns.extend(
        TEXT_COLUMNS
            .iter()
            .map(|(_, header)| Column::new(header).number()),
    );
    columns.push(Column::new("COMPOSITE/100").number());
    columns.push(Column::new("SUPPORT").fixed());
    let mut table =
        Table::new(columns).empty_message("no task in the window names an orchestrator");
    for row in value["rows"].as_array().into_iter().flatten() {
        let mut cells = vec![text(&row["orchestrator"])];
        cells.extend(
            TEXT_COLUMNS
                .iter()
                .map(|(key, _)| number(&row["counts"][key])),
        );
        cells.push(number(&row["composite"]["per_100_tasks"]));
        cells.push(if row["below_support"] == true {
            "below".into()
        } else {
            "ok".into()
        });
        table.add_row(cells);
    }
    vec![
        Block::text(selection_line(value)),
        Block::table(table),
        Block::text(format!(
            "\nComposite: {}\nWeights: {}",
            text(&value["definitions"]["composite"]["definition"]),
            weights(value),
        )),
    ]
}

fn show_blocks(value: &Value) -> Vec<Block> {
    let row = &value["row"];
    let support = &row["support"];
    let mut blocks = vec![Block::text(format!(
        "{}{}\n{}\n{} owned tasks · {} with runs · {} active hours",
        text(&value["orchestrator"]),
        if row["below_support"] == true {
            " (below minimum support)"
        } else {
            ""
        },
        selection_line(value),
        number(&support["owned_tasks"]),
        number(&support["tasks_with_runs"]),
        number(&support["active_hours"]),
    ))];

    let mut table = Table::new(vec![
        Column::new("COMPONENT").fixed(),
        Column::new("COUNT").number(),
        Column::new("PER 100").number(),
        Column::new("PER 8H").number(),
    ]);
    let components = value["definitions"]["components"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for component in &components {
        let key = text(&component["key"]);
        table.add_row(vec![
            text(&component["label"]),
            number(&row["counts"][&key]),
            number(&row["per_100_tasks"][&key]),
            number(&row["per_8_hours"][&key]),
        ]);
    }
    table.add_row(vec![
        "Composite".into(),
        number(&row["composite"]["value"]),
        number(&row["composite"]["per_100_tasks"]),
        number(&row["composite"]["per_8_hours"]),
    ]);
    blocks.push(Block::table(table));

    let mut out = String::new();
    for component in value["components"].as_array().into_iter().flatten() {
        let tasks = component["tasks"].as_array().cloned().unwrap_or_default();
        if tasks.is_empty() || component["key"] == "owned" {
            continue;
        }
        let _ = writeln!(
            out,
            "\n{} ({})",
            text(&component["label"]),
            number(&component["count"])
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
                let cause = occurrence["cause"]
                    .as_str()
                    .map(|cause| format!(" [{cause}]"))
                    .unwrap_or_default();
                let _ = writeln!(
                    out,
                    "    {}{cause}  {}",
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

fn selection_line(value: &Value) -> String {
    format!(
        "window {} · minimum support {} · {} attributed tasks · {} without an orchestrator · computed {}",
        text(&value["window"]),
        number(&value["min_support"]),
        number(&value["attributed_tasks"]),
        number(&value["unattributed_tasks"]),
        text(&value["computed_at"]),
    )
}

fn weights(value: &Value) -> String {
    value["definitions"]["composite"]["weights"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|weight| format!("{} × {}", number(&weight["weight"]), text(&weight["key"])))
        .collect::<Vec<_>>()
        .join(", ")
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_string()
}

/// A count or rate; `-` when the payload has none (no active hours).
fn number(value: &Value) -> String {
    match value {
        Value::Number(number) if number.is_f64() => {
            let rate = number.as_f64().unwrap_or(0.0);
            if rate.fract() == 0.0 {
                format!("{rate:.0}")
            } else {
                format!("{rate:.1}")
            }
        }
        Value::Number(number) => number.to_string(),
        _ => "-".into(),
    }
}
