use std::fmt::Write as _;

use serde_json::Value;

use super::support::text;
use crate::output::common::payload::Block;

pub(super) fn block(value: &Value) -> Block {
    Block::text(control_text(value))
}

fn control_text(value: &Value) -> String {
    let mut out = format!(
        "Resident {} is {} after {} by {}.",
        text(&value["task_id"]),
        text(&value["state"]),
        text(&value["action"]),
        text(&value["by"]),
    );
    match value["handoff"].as_str() {
        Some("review") => out.push_str(" The assignment moved to review."),
        Some("cycle_live") => {
            out.push_str(
                " The live cycle finishes first; its finalize moves the assignment to review.",
            );
        }
        Some(other) if other.starts_with("refused") => {
            let _ = write!(out, " The assignment did not move to review ({other}).");
        }
        _ => {}
    }
    out
}
