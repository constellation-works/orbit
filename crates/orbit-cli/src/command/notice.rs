//! The fleet notice CLI, derived from the notice operation registry.
//!
//! Every verb calls its `orbit.notice.*` tool, so `--json` is the tool's
//! payload. What is left here is turning the CLI-only flags (`--body-file`,
//! `--keep-after-done`, `--evidence TYPE=VALUE`) into tool input, and the text
//! rendering: a table for `list`, the notice for every other verb.

use std::fmt::Write as _;

use chrono::{DateTime, Utc};
use clap::Args;
use orbit_common::governance::notice::NoticeVerb;
use orbit_core::{OrbitError, OrbitRuntime};
use serde_json::{Map, Value, json};

use super::operation_args::Invocation;
use crate::command::{CommandOut, Execute, Payload};
use crate::output::payload::Block;
use crate::output::table::{Column, Table};

#[derive(Args)]
#[command(about = orbit_common::governance::notice::NOTICE.cli_about)]
pub struct NoticeCommand {
    #[command(subcommand)]
    pub command: Invocation<NoticeVerb>,
}

impl Execute for NoticeCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let Invocation { spec, mut input } = self.command;
        if spec.verb == NoticeVerb::Post {
            post_cli_input(&mut input)?;
        }
        let value = runtime.run_tool(spec.tool_name, Value::Object(input))?;
        Ok(render_notice(spec.verb, value).into())
    }
}

/// The text rendering of one notice tool answer, shared by the local
/// command and a call host routing delivered to the workspace's owner.
pub(crate) fn render_notice(verb: NoticeVerb, value: Value) -> Payload {
    let blocks = match verb {
        NoticeVerb::List => vec![Block::table(list_table(&value, Utc::now()))],
        NoticeVerb::Post => vec![Block::text(format!(
            "{} {}\n{}",
            text(&value["outcome"]),
            text(&value["notice"]["id"]),
            notice_detail(&value["notice"]),
        ))],
        NoticeVerb::Close => {
            let verb = if value["already_inactive"].as_bool() == Some(true) {
                "already inactive"
            } else {
                "closed"
            };
            vec![Block::text(format!(
                "{verb} {}\n{}",
                text(&value["notice"]["id"]),
                notice_detail(&value["notice"]),
            ))]
        }
        NoticeVerb::Show => vec![Block::text(show_text(&value))],
    };
    Payload::blocks(value, blocks)
}

/// Fold the CLI-only post flags into the tool's input.
pub(crate) fn post_cli_input(input: &mut Map<String, Value>) -> Result<(), OrbitError> {
    if let Some(path) = input.remove("body_file") {
        let path = path.as_str().unwrap_or_default().to_string();
        let body = std::fs::read_to_string(&path).map_err(|error| {
            OrbitError::InvalidInput(format!("cannot read --body-file {path}: {error}"))
        })?;
        input.insert("body".into(), Value::String(body));
    }
    if input
        .remove("keep_after_done")
        .and_then(|value| value.as_bool())
        == Some(true)
    {
        input.insert("close_on_linked_done".into(), Value::Bool(false));
    }
    if let Some(refs) = input.remove("evidence_refs") {
        let mut evidence = Vec::new();
        for item in refs.as_array().into_iter().flatten() {
            let raw = item.as_str().unwrap_or_default();
            let Some((kind, value)) = raw.split_once('=') else {
                return Err(OrbitError::InvalidInput(format!(
                    "--evidence takes TYPE=VALUE, got `{raw}`"
                )));
            };
            evidence.push(json!({"type": kind.trim(), "value": value.trim()}));
        }
        input.insert("evidence".into(), Value::Array(evidence));
    }
    Ok(())
}

fn text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn timestamp(value: &Value) -> Option<DateTime<Utc>> {
    value
        .as_str()
        .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
        .map(|at| at.with_timezone(&Utc))
}

/// A coarse duration: `45m`, `3h`, `2d`.
fn span(seconds: i64) -> String {
    let seconds = seconds.max(0);
    if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h", seconds / 3600)
    } else {
        format!("{}d", seconds / 86_400)
    }
}

fn list_table(value: &Value, now: DateTime<Utc>) -> Table {
    let mut table = Table::new(vec![
        Column::new("ID").fixed(),
        Column::new("KIND"),
        Column::new("SCOPE"),
        Column::new("TITLE"),
        Column::new("AUTHOR"),
        Column::new("AGE"),
        Column::new("EXPIRES"),
    ])
    .empty_message("no notices");
    for notice in value["notices"].as_array().into_iter().flatten() {
        let age = timestamp(&notice["opened_at"])
            .map(|at| span((now - at).num_seconds()))
            .unwrap_or_default();
        let expires = match notice["status"].as_str() {
            Some("open") => timestamp(&notice["expires_at"])
                .map(|at| format!("in {}", span((at - now).num_seconds())))
                .unwrap_or_default(),
            other => other.unwrap_or_default().to_string(),
        };
        table.add_row(vec![
            text(&notice["id"]),
            text(&notice["kind"]),
            text(&notice["scope"]),
            text(&notice["title"]),
            text(&notice["author"]["class"]),
            age,
            expires,
        ]);
    }
    if let Some(degraded) = value["degraded"]
        .as_array()
        .filter(|homes| !homes.is_empty())
    {
        let homes: Vec<String> = degraded.iter().map(text).collect();
        table = table.trailing_notice(format!("not read: {}", homes.join(", ")));
    }
    table
}

fn notice_detail(notice: &Value) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{} {} notice: {}",
        text(&notice["scope"]),
        text(&notice["kind"]),
        text(&notice["title"])
    );
    let _ = writeln!(
        out,
        "status {}, expires {}, reports {}",
        text(&notice["status"]),
        text(&notice["expires_at"]),
        text(&notice["reports"]["count"])
    );
    let author = &notice["author"];
    let mut by = text(&author["class"]);
    for field in ["crew", "task_id", "run_id"] {
        if let Some(value) = author[field].as_str() {
            let _ = write!(by, " {value}");
        }
    }
    let _ = write!(out, "by {by}");
    if let Some(linked) = notice["linked_task"].as_object() {
        let _ = write!(
            out,
            "\nlinked task {}{}",
            text(&linked["id"]),
            linked
                .get("status")
                .and_then(Value::as_str)
                .map(|status| format!(" ({status})"))
                .unwrap_or_default()
        );
    }
    if let Some(closed) = notice["closed"].as_object() {
        let _ = write!(
            out,
            "\nclosed {} by {}{}",
            text(&closed["at"]),
            text(&closed["by"]["class"]),
            closed
                .get("reason")
                .and_then(Value::as_str)
                .map(|reason| format!(": {reason}"))
                .unwrap_or_default()
        );
    }
    out
}

fn show_text(value: &Value) -> String {
    let notice = &value["notice"];
    let mut out = format!("{}\n{}", text(&notice["id"]), notice_detail(notice));
    for item in notice["evidence"].as_array().into_iter().flatten() {
        let _ = write!(
            out,
            "\nevidence {}: {}",
            text(&item["type"]),
            text(&item["value"])
        );
    }
    let hosts: Vec<String> = notice["hosts"]
        .as_array()
        .into_iter()
        .flatten()
        .map(text)
        .collect();
    if !hosts.is_empty() {
        let _ = write!(out, "\nhosts {}", hosts.join(", "));
    }
    if let Some(body) = notice["body"].as_str().filter(|body| !body.is_empty()) {
        let _ = write!(out, "\n\n{body}");
    }
    if let Some(history) = value["history"].as_array() {
        out.push_str("\n\nhistory:");
        for event in history {
            let _ = write!(
                out,
                "\n  {} {} by {}",
                text(&event["at"]),
                text(&event["action"]),
                text(&event["poster_class"])
            );
        }
    }
    out
}
