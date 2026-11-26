use std::fmt::Write as _;

use serde_json::Value;

use super::support::{number, text};
use crate::output::common::payload::Block;
use crate::output::common::table::{Column, Table};

pub(super) fn blocks(value: &Value) -> Vec<Block> {
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
        "\nPrimary metric: {}\nEnrollment: {} analysed, {} forced (excluded), {} surface-conflict exclusions, {} explicit-crew exclusions, {} interactive exclusions\nSample ratio: chi-square {}, df {}, p = {}{}\nLagging: {} task(s) in flight, {} regression window(s) open",
        text(&experiment["primary_metric"]),
        text(&enrollment["analysed"]),
        text(&enrollment["forced_excluded"]),
        text(&enrollment["surface_conflict_excluded"]),
        text(&enrollment["explicit_crew_excluded"]),
        text(&enrollment["interactive_excluded"]),
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
        Column::new("EXPLORER").number(),
        Column::new("DELIVERED"),
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
            text(&arm["tasks_with_explorer_call"]),
            delivered_cell(&arm["delivery"]),
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

    // Shadow memory arms: what was captured, never delivered.
    let has_captures = value["arms"].as_array().into_iter().flatten().any(|arm| {
        arm["memory_captures"]
            .as_array()
            .is_some_and(|captures| !captures.is_empty())
    });
    if has_captures {
        blocks.push(Block::text("\nMemory captures (captured, not delivered):"));
        let mut captures = Table::new(vec![
            Column::new("ARM"),
            Column::new("TASK"),
            Column::new("DIGEST"),
            Column::new("MODEL REV"),
            Column::new("GLOBAL REV"),
            Column::new("RENDERER").number(),
            Column::new("WEEK"),
        ]);
        for arm in value["arms"].as_array().into_iter().flatten() {
            for capture in arm["memory_captures"].as_array().into_iter().flatten() {
                captures.add_row(vec![
                    text(&arm["name"]),
                    text(&capture["task_id"]),
                    text(&capture["digest"]),
                    text(&capture["model_revision"]),
                    text(&capture["global_revision"]),
                    text(&capture["renderer_version"]),
                    text(&capture["week"]),
                ]);
            }
        }
        blocks.push(Block::table(captures));
        for arm in value["arms"].as_array().into_iter().flatten() {
            let strata = arm["memory_capture_strata"].as_array();
            if let Some(strata) = strata.filter(|strata| !strata.is_empty()) {
                let summary = strata
                    .iter()
                    .map(|stratum| {
                        format!(
                            "{}: {} task(s), digests {}",
                            text(&stratum["week"]),
                            text(&stratum["tasks"]),
                            stratum["digests"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .map(text)
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("; ");
                blocks.push(Block::text(format!(
                    "Arm {} captured (not delivered): {}",
                    text(&arm["name"]),
                    summary
                )));
            }
        }
    }

    let per_protocol = &value["per_protocol"];
    if per_protocol.is_object() {
        let metric = &per_protocol["metric"];
        // The table that follows is the per-protocol primary metric (delivered
        // runs only). Intention-to-treat metrics are emitted later, each under
        // its own heading, so this sentence must not describe them.
        blocks.push(Block::text(format!(
            "\nPrimary metric {}, {}:",
            text(&metric["metric"]),
            text(&per_protocol["label"]),
        )));
        let mut table = Table::new(vec![
            Column::new("ARM"),
            Column::new("N").number(),
            Column::new("MEAN [95%]"),
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
                with_interval(&arm["mean"], &arm["mean_interval"]),
                difference.map_or_else(String::new, |difference| {
                    with_interval(
                        &difference["overall"]["estimate"],
                        &difference["overall"]["interval"],
                    )
                }),
            ]);
        }
        blocks.push(Block::table(table));
    }

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
            empty.push(format!(
                "{}{}",
                text(&metric["metric"]),
                if metric["exploratory"] == true {
                    " (exploratory)"
                } else {
                    ""
                }
            ));
            continue;
        }
        let rate = metric["kind"] == "rate";
        blocks.push(Block::text(format!(
            "\n{}{} ({}, {} is better, {}):",
            text(&metric["metric"]),
            if metric["exploratory"] == true {
                " (exploratory)"
            } else {
                ""
            },
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

/// An arm's delivered tasks over its settled ones, with the rate and any
/// tasks still pending; blank for an arm without a delivery check.
fn delivered_cell(delivery: &Value) -> String {
    if !delivery.is_object() {
        return String::new();
    }
    let settled = delivery["assigned"]
        .as_u64()
        .unwrap_or(0)
        .saturating_sub(delivery["pending"].as_u64().unwrap_or(0));
    let mut cell = format!("{}/{settled}", text(&delivery["delivered"]));
    if let Some(rate) = delivery["rate"].as_f64() {
        let _ = write!(cell, " ({:.0}%)", rate * 100.0);
    }
    if delivery["pending"]
        .as_u64()
        .is_some_and(|pending| pending > 0)
    {
        let _ = write!(cell, ", {} pending", text(&delivery["pending"]));
    }
    cell
}

/// `estimate [low, high]`, or the estimate alone without an interval.
fn with_interval(estimate: &Value, interval: &Value) -> String {
    let estimate = number(estimate);
    match interval.as_array().map(Vec::as_slice) {
        Some([low, high]) => format!("{estimate} [{}, {}]", number(low), number(high)),
        _ => estimate,
    }
}
