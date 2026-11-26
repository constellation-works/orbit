use std::fmt::Write as _;

use serde_json::Value;

use super::support::{joined, text};
use crate::output::common::payload::Block;

pub(super) fn block(value: &Value) -> Block {
    Block::text(grant_text(value))
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
