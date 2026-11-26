use std::fmt::Write as _;

use serde_json::Value;

use super::support::{joined, text};
use crate::output::common::payload::Block;

pub(super) fn block(value: &Value) -> Block {
    Block::text(show_text(value))
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
    if let Some(reason) = value["stalled_reason"].as_str() {
        let _ = writeln!(out, "Stalled: {reason}");
    }
    if !value["status_line"].is_null() {
        let line = &value["status_line"];
        let _ = writeln!(
            out,
            "Status{}: {} (posted {})",
            if line["stale"].as_bool() == Some(true) {
                " (silent since it lapsed)"
            } else {
                ""
            },
            text(&line["title"]),
            text(&line["posted_at"]),
        );
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
