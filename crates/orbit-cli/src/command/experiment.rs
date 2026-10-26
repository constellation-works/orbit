//! The experiment CLI, derived from the experiment operation registry.
//!
//! Every verb calls its `orbit.experiment.*` tool, so `--json` is the tool's
//! payload. What is left here is the text rendering: a table for `list`, and
//! for every other verb the experiment's definition, arms and, for `show`,
//! its event log in order.

use std::fmt::Write as _;

use clap::Args;
use orbit_common::governance::experiment::ExperimentVerb;
use orbit_core::OrbitRuntime;
use serde_json::Value;

use super::operation_args::Invocation;
use crate::command::{CommandOut, Execute, Payload};
use crate::output::payload::Block;
use crate::output::table::{Column, Table};

#[derive(Args)]
#[command(about = orbit_common::governance::experiment::EXPERIMENT.cli_about)]
pub struct ExperimentCommand {
    #[command(subcommand)]
    pub command: Invocation<ExperimentVerb>,
}

impl Execute for ExperimentCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let Invocation { spec, input } = self.command;
        let value = runtime.run_tool(spec.tool_name, Value::Object(input))?;
        let blocks = match spec.verb {
            ExperimentVerb::Assign => vec![Block::text(value.to_string())],
            ExperimentVerb::List => vec![Block::table(list_table(&value))],
            ExperimentVerb::Create
            | ExperimentVerb::Update
            | ExperimentVerb::Clone
            | ExperimentVerb::Start
            | ExperimentVerb::Pause
            | ExperimentVerb::Resume
            | ExperimentVerb::Reweight
            | ExperimentVerb::Conclude
            | ExperimentVerb::Abandon
            | ExperimentVerb::Show => experiment_blocks(&value),
        };
        Ok(Payload::blocks(value, blocks).into())
    }
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

fn experiment_blocks(value: &Value) -> Vec<Block> {
    let mut header = format!(
        "{} {} — {} ({}), segment {}",
        text(&value["id"]),
        text(&value["name"]),
        text(&value["status"]),
        text(&value["mode"]),
        text(&value["segment"]),
    );
    let fields = [
        ("Hypothesis", text(&value["hypothesis"])),
        ("Primary metric", text(&value["primary_metric"])),
        ("Guardrails", compact(&value["guardrails"])),
        ("Population", compact(&value["population"])),
        ("Min sample per arm", text(&value["min_sample"])),
        ("Max days", text(&value["max_days"])),
        ("Concluded arm", text(&value["concluded_arm"])),
        ("Cloned from", text(&value["cloned_from"])),
    ];
    for (label, field) in fields {
        if !field.is_empty() && field != "{}" {
            let _ = write!(header, "\n{label}: {field}");
        }
    }
    let mut blocks = vec![Block::text(header)];

    let mut arms = Table::new(vec![
        Column::new("ARM"),
        Column::new("CONTROL"),
        Column::new("WEIGHT").number(),
        Column::new("RETIRED AT"),
        Column::new("OVERLAY"),
    ])
    .empty_message("no arms defined");
    for arm in value["arms"].as_array().into_iter().flatten() {
        arms.add_row(vec![
            text(&arm["name"]),
            if arm["control"] == Value::Bool(true) {
                "control".to_string()
            } else {
                String::new()
            },
            text(&arm["weight"]),
            text(&arm["retired_segment"]),
            compact(&arm["overlay"]),
        ]);
    }
    blocks.push(Block::text("\nArms:"));
    blocks.push(Block::table(arms));

    let events = value["events"]
        .as_array()
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    if !events.is_empty() {
        let mut table = Table::new(vec![
            Column::new("SEQ").number(),
            Column::new("EVENT"),
            Column::new("STATUS"),
            Column::new("SEGMENT").number(),
            Column::new("ACTOR"),
            Column::new("AT"),
            Column::new("REASON"),
        ]);
        for event in events {
            let status = match (event["from_status"].as_str(), event["to_status"].as_str()) {
                (Some(from), Some(to)) if from != to => format!("{from} → {to}"),
                (_, Some(to)) => to.to_string(),
                _ => String::new(),
            };
            table.add_row(vec![
                text(&event["seq"]),
                text(&event["kind"]),
                status,
                text(&event["segment"]),
                text(&event["actor"]),
                text(&event["at"]),
                text(&event["reason"]),
            ]);
        }
        blocks.push(Block::text("\nEvents:"));
        blocks.push(Block::table(table));
    }
    blocks
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

fn text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn compact(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        other => other.to_string(),
    }
}
