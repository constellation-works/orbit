use std::fmt::Write as _;

use serde_json::Value;

use super::support::text;
use crate::output::common::payload::Block;

pub(super) fn block(value: &Value) -> Block {
    Block::text(digest_text(value))
}

fn digest_text(value: &Value) -> String {
    let mut out = format!("Resident digest since {}\n", text(&value["since"]));
    for resident in value["residents"].as_array().into_iter().flatten() {
        let _ = writeln!(
            out,
            "{} [{}]: {}, {} open request(s){}",
            text(&resident["title"]),
            text(&resident["state"]),
            text(&resident["last_outcome"]),
            text(&resident["open_request_count"]),
            if resident["stalled"] == true {
                " (stalled)"
            } else {
                ""
            }
        );
    }
    for (field, label) in [
        ("open_escalations", "Open escalations"),
        ("decisions_awaiting_veto", "Decisions awaiting veto"),
        ("open_proposals", "Proposals awaiting approval"),
    ] {
        let rows: Vec<&Value> = value[field].as_array().into_iter().flatten().collect();
        let _ = writeln!(out, "{label}: {}", rows.len());
        for row in rows {
            let content = row
                .get("question")
                .or_else(|| row.get("summary"))
                .or_else(|| row.get("title"))
                .unwrap_or(&Value::Null);
            let _ = writeln!(out, "  {}", text(content));
        }
    }
    let _ = writeln!(out, "Top pains:");
    for pain in value["top_pains"].as_array().into_iter().flatten() {
        let _ = writeln!(
            out,
            "  {}: {} minute(s), {} occurrence(s), {} cycle(s), {} mandate(s)",
            text(&pain["title"]),
            text(&pain["summed_cost_minutes"]),
            text(&pain["occurrence_count"]),
            text(&pain["cycle_count"]),
            text(&pain["mandate_count"])
        );
    }
    let _ = writeln!(
        out,
        "Metrics: {} intervention(s), {} escalation(s)",
        text(&value["metrics"]["interventions"]["count"]),
        text(&value["metrics"]["escalations"]["count"])
    );
    out.trim_end().to_string()
}
