//! The resident mandate CLI, derived from the resident operation registry.
//!
//! Every verb calls its `orbit.resident.*` tool, so `--json` is the tool's
//! payload; what is left here is the text rendering.

use std::fmt::Write as _;

use clap::Args;
use orbit_common::governance::resident::ResidentVerb;
use orbit_core::OrbitRuntime;
use serde_json::Value;

use super::operation_args::Invocation;
use crate::command::{CommandOut, Execute, Payload};
use crate::output::payload::Block;
use crate::output::table::{Column, Table};

#[derive(Args)]
#[command(about = orbit_common::governance::resident::RESIDENT.cli_about)]
pub struct ResidentCommand {
    #[command(subcommand)]
    pub command: Invocation<ResidentVerb>,
}

impl Execute for ResidentCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let Invocation { spec, input } = self.command;
        let value = runtime.run_tool(spec.tool_name, Value::Object(input))?;
        let blocks = match spec.verb {
            ResidentVerb::Checkpoint => vec![Block::text(format!(
                "Checkpoint recorded for cycle {}.",
                text(&value["checkpoint"]["cycle"])
            ))],
            ResidentVerb::Grant => vec![Block::text(grant_text(&value))],
            ResidentVerb::Metrics => vec![Block::text(metrics_text(&value))],
            ResidentVerb::Show => vec![Block::text(show_text(&value))],
            ResidentVerb::List => vec![Block::table(list_table(&value))],
        };
        Ok(Payload::blocks(value, blocks).into())
    }
}

fn text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn joined(value: &Value) -> String {
    let items: Vec<String> = value.as_array().into_iter().flatten().map(text).collect();
    if items.is_empty() {
        "none".to_string()
    } else {
        items.join(", ")
    }
}

fn authority_line(authority: &Value) -> String {
    format!(
        "rights: {}; workspaces: {}; revision {}",
        joined(&authority["rights"]),
        joined(&authority["workspaces"]),
        text(&authority["revision"]),
    )
}

fn grant_text(value: &Value) -> String {
    let mut out = String::new();
    match value["outcome"].as_str().unwrap_or_default() {
        "requested" => {
            let request = &value["request"];
            let verb = if value["deduplicated"].as_bool() == Some(true) {
                "Grant request already open"
            } else {
                "Raised grant request"
            };
            let _ = writeln!(
                out,
                "{verb} {}: {}\nThe authority is unchanged until a human answers it (orbit decision answer {} grant|reject).",
                text(&request["id"]),
                text(&request["summary"]),
                text(&request["id"]),
            );
        }
        other => {
            let _ = writeln!(out, "Authority {other}.");
        }
    }
    let _ = write!(out, "Authority: {}", authority_line(&value["authority"]));
    out
}

fn measure_line(label: &str, measure: &Value) -> String {
    if let Some(text_value) = measure.as_str() {
        return format!("{label}: {text_value}");
    }
    format!(
        "{label}: {} (per mandate-day {})",
        text(&measure["count"]),
        text(&measure["per_mandate_day"]),
    )
}

fn metrics_text(value: &Value) -> String {
    let mut out = String::new();
    let window = &value["window"];
    let _ = writeln!(
        out,
        "Resident metrics since {} ({} day(s))",
        text(&window["start"]),
        text(&window["days"]),
    );
    let mut sections: Vec<(&str, &Value)> = Vec::new();
    for mandate in value["mandates"].as_array().into_iter().flatten() {
        sections.push((mandate["task_id"].as_str().unwrap_or_default(), mandate));
    }
    if !value["totals"].is_null() && sections.len() != 1 {
        sections.push(("totals", &value["totals"]));
    }
    if !value["baseline"].is_null() {
        sections.push(("baseline", &value["baseline"]));
    }
    for (label, mandate) in sections {
        let _ = writeln!(out, "{label}:");
        let _ = writeln!(
            out,
            "  {}",
            measure_line("interventions", &mandate["interventions"])
        );
        let escalations = &mandate["escalations"];
        let _ = writeln!(
            out,
            "  escalation load: {} (per mandate-day {}, median answer latency {}s, recommended share {}, expired {})",
            text(&escalations["count"]),
            text(&escalations["per_mandate_day"]),
            text(&escalations["median_answer_latency_seconds"]),
            text(&escalations["recommended_share"]),
            text(&escalations["expired_unanswered"]),
        );
        if let Some(marker) = mandate["reversals"].as_str() {
            let _ = writeln!(out, "  reversals: {marker}");
        } else {
            let _ = writeln!(
                out,
                "  reversals: {}/{} (rate {})",
                text(&mandate["reversals"]["reversed"]),
                text(&mandate["reversals"]["total"]),
                text(&mandate["reversals"]["rate"]),
            );
        }
        if let Some(marker) = mandate["recovery"].as_str() {
            let _ = writeln!(out, "  recovery: {marker}");
        } else {
            let _ = writeln!(
                out,
                "  recovery: {} crashed cycle(s), {} repeated action(s), {} slot minute(s)",
                text(&mandate["recovery"]["crashed_cycles"]),
                text(&mandate["recovery"]["repeated_actions"]),
                text(&mandate["recovery"]["recovery_slot_minutes"]),
            );
        }
    }
    out.trim_end().to_string()
}

fn entry_line(entry: &Value) -> String {
    let mut changes = Vec::new();
    for (field, label) in [
        ("added", "+"),
        ("workspaces_added", "+workspace "),
        ("removed", "-"),
        ("workspaces_dropped", "-workspace "),
    ] {
        for item in entry[field].as_array().into_iter().flatten() {
            changes.push(format!("{label}{}", text(item)));
        }
    }
    if !entry["deadline"].is_null() {
        changes.push(format!("deadline {}", text(&entry["deadline"])));
    }
    let mut line = format!(
        "  r{} {} by {} via {}: {}",
        text(&entry["revision"]),
        text(&entry["at"]),
        text(&entry["by"]),
        text(&entry["via"]),
        changes.join(" "),
    );
    if let Some(quote) = entry["quote"].as_str() {
        let _ = write!(line, "\n    quote: {quote}");
    }
    line
}

fn show_text(value: &Value) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{} {} [{}]",
        text(&value["task_id"]),
        text(&value["title"]),
        text(&value["state"]),
    );
    if let Some(reason) = value["state_reason"].as_str() {
        let _ = writeln!(out, "Not due: {reason}");
    }
    if !value["latest_checkpoint"].is_null() {
        let checkpoint = &value["latest_checkpoint"];
        let _ = writeln!(
            out,
            "Latest checkpoint: {} [{}]\n{}",
            text(&checkpoint["cycle"]),
            text(&checkpoint["outcome"]),
            text(&checkpoint["notes"])
        );
    }
    let mandate = &value["mandate"];
    if !mandate.is_null() {
        let _ = writeln!(out, "Goal: {}", text(&mandate["goal"]));
        if !mandate["deadline"].is_null() {
            let _ = writeln!(out, "Deadline: {}", text(&mandate["deadline"]));
        }
        let _ = writeln!(
            out,
            "Mandate workspaces: {}",
            joined(&mandate["workspaces"])
        );
    }
    let effective = &value["effective_authority"];
    let _ = writeln!(
        out,
        "Effective authority: rights {}; workspaces {}",
        joined(&effective["rights"]),
        joined(&effective["workspaces"]),
    );
    if let Some(error) = value["authority_error"].as_str() {
        let _ = writeln!(out, "Authority record: {error}");
    }
    let authority = &value["authority"];
    let history: Vec<&Value> = authority["granted"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(authority["narrowed"].as_array().into_iter().flatten())
        .collect();
    let mut history = history;
    history.sort_by_key(|entry| entry["revision"].as_u64().unwrap_or_default());
    if !history.is_empty() {
        let _ = writeln!(out, "History:");
        for entry in history {
            let _ = writeln!(out, "{}", entry_line(entry));
        }
    }
    for request in value["open_requests"].as_array().into_iter().flatten() {
        let _ = writeln!(
            out,
            "Open grant request {}: {}",
            text(&request["id"]),
            text(&request["summary"]),
        );
    }
    out.trim_end().to_string()
}

fn list_table(value: &Value) -> Table {
    let mut table = Table::new(vec![
        Column::new("TASK").fixed(),
        Column::new("STATE"),
        Column::new("RIGHTS"),
        Column::new("WORKSPACES"),
        Column::new("REQUESTS"),
        Column::new("GOAL"),
    ])
    .empty_message("no resident mandates");
    for row in value["mandates"].as_array().into_iter().flatten() {
        table.add_row(vec![
            text(&row["task_id"]),
            text(&row["state"]),
            joined(&row["rights"]),
            joined(&row["workspaces"]),
            text(&row["open_requests"]),
            text(&row["goal"]),
        ]);
    }
    table
}
