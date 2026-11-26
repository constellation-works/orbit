use std::fmt::Write as _;

use serde_json::Value;

use super::support::text;
use crate::output::common::payload::Block;

pub(super) fn block(value: &Value) -> Block {
    Block::text(metrics_text(value))
}

/// A metric for plain text. Null means nothing was measured (no answered escalation, no
/// reversal denominator), so it prints `n/a` rather than an empty slot.
fn metric_value(value: &Value) -> String {
    if value.is_null() {
        "n/a".to_string()
    } else {
        text(value)
    }
}

/// [`metric_value`] for a duration in seconds: the unit follows a measured value only.
fn metric_seconds(value: &Value) -> String {
    if value.is_null() {
        "n/a".to_string()
    } else {
        format!("{}s", text(value))
    }
}

fn measure_line(label: &str, measure: &Value) -> String {
    if let Some(text_value) = measure.as_str() {
        return format!("{label}: {text_value}");
    }
    format!(
        "{label}: {} (per mandate-day {})",
        metric_value(&measure["count"]),
        metric_value(&measure["per_mandate_day"]),
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
            "  escalation load: {} (per mandate-day {}, per human-day {}, median answer latency {}, recommended share {}, expired {}, superseded {})",
            metric_value(&escalations["count"]),
            metric_value(&escalations["per_mandate_day"]),
            metric_value(&escalations["per_human_day"]),
            metric_seconds(&escalations["median_answer_latency_seconds"]),
            metric_value(&escalations["recommended_share"]),
            metric_value(&escalations["expired_unanswered"]),
            metric_value(&escalations["superseded"]),
        );
        if let Some(marker) = mandate["reversals"].as_str() {
            let _ = writeln!(out, "  reversals: {marker}");
        } else {
            let _ = writeln!(
                out,
                "  reversals: {}/{} (rate {})",
                metric_value(&mandate["reversals"]["reversed"]),
                metric_value(&mandate["reversals"]["total"]),
                metric_value(&mandate["reversals"]["rate"]),
            );
        }
        if let Some(marker) = mandate["recovery"].as_str() {
            let _ = writeln!(out, "  recovery: {marker}");
        } else {
            let _ = writeln!(
                out,
                "  recovery: {} crashed cycle(s), {} repeated action(s), {} slot minute(s)",
                metric_value(&mandate["recovery"]["crashed_cycles"]),
                metric_value(&mandate["recovery"]["repeated_actions"]),
                metric_value(&mandate["recovery"]["recovery_slot_minutes"]),
            );
        }
    }
    out.trim_end().to_string()
}
