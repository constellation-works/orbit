//! The crew CLI, derived from the crew operation registry.
//!
//! `ladder` calls the read-only `orbit.crew.ladder` tool, so `--json` is that
//! tool's payload. What is left here is the text rendering: the ladder line,
//! one row per complexity tier, and each tier's reason when it has no trial.

use clap::Args;
use orbit_common::governance::crew::CrewVerb;
use orbit_core::OrbitRuntime;
use serde_json::Value;

use super::operation_args::Invocation;
use crate::command::{CommandOut, Execute, Payload};
use crate::output::payload::Block;
use crate::output::table::{Column, Table};

#[derive(Args)]
#[command(about = orbit_common::governance::crew::CREW.cli_about)]
pub struct CrewCommand {
    #[command(subcommand)]
    pub command: Invocation<CrewVerb>,
}

impl Execute for CrewCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let Invocation { spec, input } = self.command;
        let value = runtime.run_tool(spec.tool_name, Value::Object(input))?;
        let blocks = match spec.verb {
            CrewVerb::Ladder => ladder_blocks(&value),
        };
        Ok(Payload::blocks(value, blocks).into())
    }
}

/// The ladder line, a row per tier, then each tier's reason.
fn ladder_blocks(value: &Value) -> Vec<Block> {
    let rungs = names(&value["rungs"], " < ");
    let tiers = names(&value["tiers"], ", ");
    let header = format!(
        "Crew ladder: mode {} · rungs (cheapest first) {} · tiers {} · explore_rate {} \
         (control {} / candidate {})\nExperiments: {}, {} of {} running",
        text(&value["mode"]),
        if rungs.is_empty() {
            "none".into()
        } else {
            rungs
        },
        if tiers.is_empty() {
            "none".into()
        } else {
            tiers
        },
        text(&value["explore_rate"]),
        text(&value["arm_weights"]["control"]),
        text(&value["arm_weights"]["candidate"]),
        if value["experiments_enabled"] == true {
            "enabled"
        } else {
            "disabled"
        },
        text(&value["running"]),
        text(&value["max_running"]),
    );
    let mut table = Table::new(vec![
        Column::new("TIER").fixed(),
        Column::new("POOL"),
        Column::new("FLOOR").fixed(),
        Column::new("CANDIDATE").fixed(),
        Column::new("TRIAL").fixed(),
        Column::new("STATUS"),
        Column::new("WOULD-BE").number(),
        Column::new("REASON"),
    ])
    .keep_all_columns();
    let mut reasons = Vec::new();
    for row in value["rows"].as_array().into_iter().flatten() {
        let trial = &row["trial"];
        let pool = row["pool"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|entry| format!("{}:{}", text(&entry["name"]), text(&entry["weight"])))
            .collect::<Vec<_>>();
        table.add_row(vec![
            text(&row["tier"]),
            if pool.is_empty() {
                "-".into()
            } else {
                pool.join(" ")
            },
            dash(&row["floor"]),
            dash(&row["candidate"]),
            dash(&trial["name"]),
            if trial.is_null() {
                "-".into()
            } else if trial["current"] == true {
                format!("{} {}", text(&trial["status"]), text(&trial["mode"]))
            } else {
                format!(
                    "{} {} (retiring)",
                    text(&trial["status"]),
                    text(&trial["mode"])
                )
            },
            dash(&trial["would_be_assignments"]),
            dash(&row["reason"]["code"]),
        ]);
        if !row["reason"].is_null() {
            reasons.push(format!(
                "  {}: {}",
                text(&row["tier"]),
                text(&row["reason"]["detail"])
            ));
        }
    }
    let mut blocks = vec![Block::text(header), Block::table(table)];
    if !reasons.is_empty() {
        blocks.push(Block::text(format!("\nReasons:\n{}", reasons.join("\n"))));
    }
    blocks
}

fn names(value: &Value, separator: &str) -> String {
    value
        .as_array()
        .into_iter()
        .flatten()
        .map(text)
        .collect::<Vec<_>>()
        .join(separator)
}

fn dash(value: &Value) -> String {
    if value.is_null() {
        "-".into()
    } else {
        text(value)
    }
}

fn text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}
