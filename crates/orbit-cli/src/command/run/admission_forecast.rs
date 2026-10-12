//! Render the recorded forecast without recalculating or reserving admission.

use serde_json::Value;

pub(super) fn admission_line(
    forecast: &Value,
    mut title: impl FnMut(&str) -> Option<String>,
) -> Option<String> {
    let mode = forecast["mode"].as_str()?;
    let label = if mode == "shadow" {
        "forecast (shadow; hypothetical)"
    } else {
        "forecast"
    };
    let mut decisions = Vec::new();
    for skipped in forecast["skipped"].as_array().into_iter().flatten() {
        let name = skipped["task_id"]
            .as_str()
            .and_then(&mut title)
            .unwrap_or_else(|| "task".to_string());
        decisions.push(format!("skipped {name} ({})", skip_detail(skipped)));
    }
    if forecast["head"]["protected"].as_bool() == Some(true) {
        let name = forecast["head"]["task_id"]
            .as_str()
            .and_then(&mut title)
            .unwrap_or_else(|| "queue head".to_string());
        decisions.push(format!("starvation guard protects {name}"));
    }
    if decisions.is_empty() && forecast["forecast_at"].is_null() {
        decisions.push("no forecast built this pass".to_string());
    }
    if decisions.is_empty() {
        let count = forecast["admitted"].as_array().map_or(0, Vec::len);
        decisions.push(format!("{count} candidate(s) fit"));
    }
    Some(format!("Admission: {label} — {}", decisions.join("; ")))
}

// Derive the explanation from structured fields: a persisted starvation
// detail names an internal task ID, which must not reach new text surfaces.
pub(super) fn skip_detail(skipped: &Value) -> String {
    match (
        skipped["resource"].as_str(),
        skipped["percent"].as_f64(),
        skipped["minute"].as_u64(),
    ) {
        (Some(resource), Some(percent), Some(minute)) => {
            format!("{resource} {percent:.0}% at +{minute}m")
        }
        _ => "waiting for protected queue head to fit (starvation guard)".to_string(),
    }
}

pub(super) fn peak_lines(forecast: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    for resource in ["cpu", "memory"] {
        let peak = &forecast[format!("{resource}_peak")];
        let Some(value) = peak["value"].as_f64() else {
            continue;
        };
        let unit = if resource == "cpu" { "cores" } else { "bytes" };
        let percent = peak["percent"]
            .as_f64()
            .map(|percent| format!(" ({percent:.1}%)"))
            .unwrap_or_else(|| " (host capacity unknown)".to_string());
        let headroom = forecast["headroom"][format!("{resource}_percent")]
            .as_f64()
            .map(|room| format!("{room:.1} percentage points under current admission limit"))
            .unwrap_or_else(|| "unknown".to_string());
        lines.push(format!(
            "Forecast peak (last admission pass): {resource} {value:.1} {unit}{percent} at +{}m; headroom: {headroom}.",
            peak["minute"]
        ));
    }
    lines
}
