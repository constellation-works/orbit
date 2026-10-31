//! The experiment CLI, derived from the experiment operation registry.
//!
//! Every verb calls its `orbit.experiment.*` tool, so `--json` is the tool's
//! payload. What is left here is the text rendering: a table for `list`, the
//! verdict, health checks and per-arm metric tables for `readout`, and for
//! every other verb the experiment's definition, arms and, for `show`, its
//! event log in order.

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
            ExperimentVerb::Readout => readout_blocks(&value),
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
        for event in &events {
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
    for event in &events {
        if event["kind"] != "concluded" {
            continue;
        }
        let detail = &event["detail"];
        if detail["readout"].is_object() {
            blocks.push(Block::text(format!(
                "\nConclusion readout snapshot (event {}): {}",
                text(&event["seq"]),
                text(&detail["readout"]["verdict"]["label"])
            )));
        }
        if let Some(task_id) = detail["promotion_task"]
            .as_str()
            .filter(|task_id| !task_id.is_empty())
        {
            blocks.push(Block::text(format!("Promotion task: {task_id}")));
        }
    }
    blocks
}

fn readout_blocks(value: &Value) -> Vec<Block> {
    let experiment = &value["experiment"];
    let verdict = &value["verdict"];
    let mut header = format!(
        "{} {} — readout (schema {}) as of {}\nVerdict: {}{}",
        text(&experiment["id"]),
        text(&experiment["name"]),
        text(&value["schema_version"]),
        text(&value["as_of"]),
        text(&verdict["label"]),
        if verdict["horizon_reached"] == Value::Bool(true) {
            " (horizon reached)"
        } else {
            ""
        },
    );
    for reason in verdict["reasons"].as_array().into_iter().flatten() {
        let _ = write!(header, "\n  - {}", text(reason));
    }
    let enrollment = &value["enrollment"];
    let ratio = &value["sample_ratio"];
    let lagging = &value["lagging"];
    let _ = write!(
        header,
        "\nPrimary metric: {}\nEnrollment: {} analysed, {} forced (excluded), {} surface-conflict exclusions, {} explicit-crew exclusions\nSample ratio: chi-square {}, df {}, p = {}{}\nLagging: {} task(s) in flight, {} regression window(s) open",
        text(&experiment["primary_metric"]),
        text(&enrollment["analysed"]),
        text(&enrollment["forced_excluded"]),
        text(&enrollment["surface_conflict_excluded"]),
        text(&enrollment["explicit_crew_excluded"]),
        number(&ratio["chi_square"]),
        text(&ratio["df"]),
        number(&ratio["p_value"]),
        if ratio["mismatch"] == Value::Bool(true) {
            " — MISMATCH"
        } else {
            ""
        },
        text(&lagging["open_tasks"]),
        text(&lagging["open_regression_windows"]),
    );
    for warning in value["warnings"].as_array().into_iter().flatten() {
        let _ = write!(
            header,
            "\nWarning [{}]: {}",
            text(&warning["kind"]),
            text(&warning["message"])
        );
    }
    let mut blocks = vec![Block::text(header)];

    let mut arms = Table::new(vec![
        Column::new("ARM"),
        Column::new("CONTROL"),
        Column::new("WEIGHT").number(),
        Column::new("N").number(),
        Column::new("SEGMENTS"),
        Column::new("CREW MIX"),
        Column::new("RECORDED EFFORT"),
        Column::new("DRIFT"),
    ])
    .empty_message("no arms defined");
    for arm in value["arms"].as_array().into_iter().flatten() {
        let segments = arm["segments"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|segment| format!("{}:{}", text(&segment["segment"]), text(&segment["n"])))
            .collect::<Vec<_>>()
            .join(" ");
        let crews = arm["crew_mix"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|crew| format!("{}:{}", text(&crew["crew"]), text(&crew["tasks"])))
            .collect::<Vec<_>>()
            .join(" ");
        arms.add_row(vec![
            text(&arm["name"]),
            if arm["control"] == Value::Bool(true) {
                "control".to_string()
            } else {
                String::new()
            },
            text(&arm["weight"]),
            text(&arm["n"]),
            segments,
            crews,
            arm["effort_breakdown"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|row| {
                    format!(
                        "{} · {} · {}: {}",
                        text(&row["crew"]),
                        text(&row["model"]),
                        if row["reasoning_effort"].is_null() {
                            "provider default".into()
                        } else {
                            text(&row["reasoning_effort"])
                        },
                        text(&row["invocations"])
                    )
                })
                .collect::<Vec<_>>()
                .join("; "),
            if arm["drifted"] == Value::Bool(true) {
                "drifted".to_string()
            } else {
                String::new()
            },
        ]);
    }
    blocks.push(Block::text("\nArms:"));
    blocks.push(Block::table(arms));

    let mut empty = Vec::new();
    for metric in value["metrics"].as_array().into_iter().flatten() {
        if metric["metric"] == "cost_usd"
            && metric["arms"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|arm| arm["cost_coverage"].is_object())
        {
            blocks.push(Block::text(
                "\nCost coverage (settled tasks; estimates use token prices, incomplete costs are excluded):",
            ));
            let mut coverage = Table::new(vec![
                Column::new("ARM"),
                Column::new("REPORTED").number(),
                Column::new("ESTIMATED").number(),
                Column::new("UNMEASURED").number(),
                Column::new("OPEN").number(),
            ]);
            for arm in metric["arms"].as_array().into_iter().flatten() {
                coverage.add_row(vec![
                    text(&arm["arm"]),
                    text(&arm["cost_coverage"]["reported"]),
                    text(&arm["cost_coverage"]["estimated"]),
                    text(&arm["cost_coverage"]["unmeasured"]),
                    text(&arm["open"]),
                ]);
            }
            blocks.push(Block::table(coverage));
        }
        let has_data = metric["arms"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|arm| arm["n"].as_u64().is_some_and(|n| n > 0));
        if !has_data {
            empty.push(text(&metric["metric"]));
            continue;
        }
        let rate = metric["kind"] == "rate";
        blocks.push(Block::text(format!(
            "\n{} ({}, {} is better, {}):",
            text(&metric["metric"]),
            text(&metric["kind"]),
            text(&metric["better"]),
            text(&metric["unit"]),
        )));
        let mut table = Table::new(vec![
            Column::new("ARM"),
            Column::new("N").number(),
            Column::new("OPEN").number(),
            Column::new(if rate {
                "RATE [95% WILSON]"
            } else {
                "MEAN [95%]"
            }),
            Column::new(if rate { "SUCCESSES" } else { "MEDIAN [95%]" }),
            Column::new("DIFF VS CONTROL [95%]"),
        ]);
        for arm in metric["arms"].as_array().into_iter().flatten() {
            let difference = metric["differences"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|difference| difference["arm"] == arm["arm"]);
            table.add_row(vec![
                text(&arm["arm"]),
                text(&arm["n"]),
                text(&arm["open"]),
                with_interval(&arm["mean"], &arm["mean_interval"]),
                if rate {
                    text(&arm["successes"])
                } else {
                    with_interval(&arm["median"], &arm["median_interval"])
                },
                difference.map_or_else(String::new, |difference| {
                    with_interval(
                        &difference["overall"]["estimate"],
                        &difference["overall"]["interval"],
                    )
                }),
            ]);
        }
        blocks.push(Block::table(table));
        let mut cells = Table::new(vec![
            Column::new("ARM"),
            Column::new("COMPLEXITY"),
            Column::new("N ARM").number(),
            Column::new("N CONTROL").number(),
            Column::new("DIFF [95%]"),
        ]);
        for difference in metric["differences"].as_array().into_iter().flatten() {
            for cell in difference["by_complexity"].as_array().into_iter().flatten() {
                cells.add_row(vec![
                    text(&difference["arm"]),
                    text(&cell["complexity"]),
                    text(&cell["n_arm"]),
                    text(&cell["n_control"]),
                    with_interval(&cell["estimate"], &cell["interval"]),
                ]);
            }
        }
        blocks.push(Block::table(cells));
    }
    if !empty.is_empty() {
        blocks.push(Block::text(format!(
            "\nNo outcomes yet: {}",
            empty.join(", ")
        )));
    }

    let guardrails = value["guardrails"]
        .as_array()
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    if !guardrails.is_empty() {
        let mut table = Table::new(vec![
            Column::new("GUARDRAIL"),
            Column::new("THRESHOLD"),
            Column::new("ARM"),
            Column::new("DIFF [95%]"),
            Column::new("STATUS"),
        ]);
        for guardrail in guardrails {
            for arm in guardrail["arms"].as_array().into_iter().flatten() {
                table.add_row(vec![
                    text(&guardrail["metric"]),
                    text(&guardrail["threshold"]),
                    text(&arm["arm"]),
                    with_interval(&arm["estimate"], &arm["interval"]),
                    text(&arm["status"]),
                ]);
            }
        }
        blocks.push(Block::text("\nGuardrails:"));
        blocks.push(Block::table(table));
    }
    blocks
}

/// A number rounded for reading; empty for null.
fn number(value: &Value) -> String {
    let Some(number) = value.as_f64() else {
        return text(value);
    };
    let magnitude = number.abs();
    if magnitude >= 100.0 {
        format!("{number:.0}")
    } else if magnitude >= 1.0 || magnitude == 0.0 {
        format!("{number:.2}")
    } else if magnitude >= 0.001 {
        format!("{number:.3}")
    } else {
        format!("{number:.2e}")
    }
}

/// `estimate [low, high]`, or the estimate alone without an interval.
fn with_interval(estimate: &Value, interval: &Value) -> String {
    let estimate = number(estimate);
    match interval.as_array().map(Vec::as_slice) {
        Some([low, high]) => format!("{estimate} [{}, {}]", number(low), number(high)),
        _ => estimate,
    }
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
