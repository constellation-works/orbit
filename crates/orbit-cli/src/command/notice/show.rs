use std::fmt::Write as _;

use serde_json::Value;

use super::support::{notice_detail, text};
use crate::output::common::payload::Block;

pub(super) fn block(value: &Value) -> Block {
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
