use std::fmt::Write as _;

use serde_json::Value;

use crate::output::common::payload::Block;
use crate::output::common::table::{Column, Table};

pub(super) fn experiment_blocks(value: &Value) -> Vec<Block> {
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
        for warning in event["detail"]["warnings"].as_array().into_iter().flatten() {
            blocks.push(Block::text(format!("Warning: {}", text(warning))));
        }
        if let Some(warning) = event["detail"]["warning"].as_str() {
            blocks.push(Block::text(format!("Warning: {warning}")));
        }
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

/// A number rounded for reading; empty for null.
pub(super) fn number(value: &Value) -> String {
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

pub(super) fn text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

pub(super) fn compact(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        other => other.to_string(),
    }
}
