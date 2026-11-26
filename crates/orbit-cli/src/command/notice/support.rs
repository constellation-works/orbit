use std::fmt::Write as _;

use chrono::{DateTime, Utc};
use serde_json::Value;

pub(super) fn text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

pub(super) fn timestamp(value: &Value) -> Option<DateTime<Utc>> {
    value
        .as_str()
        .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
        .map(|at| at.with_timezone(&Utc))
}

/// A coarse duration: `45m`, `3h`, `2d`.
pub(super) fn span(seconds: i64) -> String {
    let seconds = seconds.max(0);
    if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h", seconds / 3600)
    } else {
        format!("{}d", seconds / 86_400)
    }
}

pub(super) fn notice_detail(notice: &Value) -> String {
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
