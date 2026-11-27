//! Human and structured output for `orbit notice` (`list`, `post`, `close`,
//! and `show`).

use std::fmt::Write as _;

use chrono::{DateTime, Utc};
use orbit_common::governance::notice::NoticeVerb;
use serde_json::Value;

use crate::output::common::payload::{Block, Payload};
use crate::output::common::table::{Column, Table};

/// The text rendering of one notice tool answer, shared by the local
/// command and a call host routing delivered to the workspace's owner.
pub(crate) fn render_notice(verb: NoticeVerb, value: Value) -> Payload {
    let blocks = match verb {
        NoticeVerb::List => vec![list_block(&value, Utc::now())],
        NoticeVerb::Post => vec![post_block(&value)],
        NoticeVerb::Close => vec![close_block(&value)],
        NoticeVerb::Show => vec![show_block(&value)],
    };
    Payload::blocks(value, blocks)
}

fn list_block(value: &Value, now: DateTime<Utc>) -> Block {
    Block::table(list_table(value, now))
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

fn post_block(value: &Value) -> Block {
    Block::text(format!(
        "{} {}\n{}",
        text(&value["outcome"]),
        text(&value["notice"]["id"]),
        notice_detail(&value["notice"]),
    ))
}

fn close_block(value: &Value) -> Block {
    let verb = if value["already_inactive"].as_bool() == Some(true) {
        "already inactive"
    } else {
        "closed"
    };
    Block::text(format!(
        "{verb} {}\n{}",
        text(&value["notice"]["id"]),
        notice_detail(&value["notice"]),
    ))
}

fn show_block(value: &Value) -> Block {
    Block::text(show_text(value))
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
