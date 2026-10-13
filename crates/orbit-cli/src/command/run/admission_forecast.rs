//! Render the recorded forecast without recalculating or reserving admission.

use serde_json::Value;

pub(super) fn admission_lines(
    current: &Value,
    last_built: &Value,
    mut title: impl FnMut(&str) -> Option<String>,
) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(line) = admission_line(current, &mut title) {
        lines.push(line);
    }
    if let Some(line) = stuck_head_line(current, &mut title) {
        lines.push(line);
    }
    let forecast = if current["forecast_at"].is_null() {
        if let Some(at) = last_built["forecast_at"].as_str() {
            if lines.is_empty() {
                lines.push("Admission: no forecast built this pass".to_string());
            }
            if let Some(line) = admission_line(last_built, &mut title) {
                lines.push(format!(
                    "Last forecast {at}: {}",
                    line.trim_start_matches("Admission: ")
                ));
            }
        }
        last_built
    } else {
        current
    };
    lines.extend(peak_lines(forecast));
    lines
}

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
    if forecast["forecast_at"].is_null() {
        decisions.push("no forecast built this pass".to_string());
    } else {
        let count = forecast["admitted"].as_array().map_or(0, Vec::len);
        decisions.push(format!("{count} candidate(s) fit"));
        for admitted in forecast["admitted"].as_array().into_iter().flatten() {
            let name = admitted["task_id"]
                .as_str()
                .and_then(&mut title)
                .unwrap_or_else(|| "task".to_string());
            decisions.push(format!("admitted {name}"));
        }
    }
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
    Some(format!("Admission: {label} — {}", decisions.join("; ")))
}

/// The head the starvation guard has protected over `head_stuck_passes`
/// passes without a better miss, while it is still the head.
fn stuck_head_line(
    forecast: &Value,
    mut title: impl FnMut(&str) -> Option<String>,
) -> Option<String> {
    let head = &forecast["head"];
    let stall = &head["stall"];
    if stall["stuck"].as_bool() != Some(true) {
        return None;
    }
    let miss = &head["miss"];
    let name = head["task_id"]
        .as_str()
        .and_then(&mut title)
        .unwrap_or_else(|| "queue head".to_string());
    let resource = miss["resource"].as_str().unwrap_or("resource");
    let profile = match miss["profile_key"].as_str() {
        Some(key) => format!(
            "profile {key} via {}",
            miss["profile_source"].as_str().unwrap_or("?")
        ),
        None => format!(
            "profile {}",
            miss["profile_source"].as_str().unwrap_or("none")
        ),
    };
    let baseline = miss["baseline_percent"].as_f64().map_or_else(
        || "unmeasured".to_string(),
        |percent| format!("{percent:.0}%"),
    );
    let label = if forecast["mode"] == "shadow" {
        "Stuck head (shadow; hypothetical)"
    } else {
        "Stuck head"
    };
    Some(format!(
        "{label}: {name} — {} ({profile}; {resource} baseline {baseline}) has not improved over {} protected passes since {}; nothing behind it is admitted until it fits.",
        skip_detail(miss),
        stall["passes"],
        stall["since"].as_str().unwrap_or("?"),
    ))
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
        let headroom = if forecast["headroom"].is_object() {
            let room = forecast["headroom"][format!("{resource}_percent")]
                .as_f64()
                .map(|room| format!("{room:.1} percentage points under current admission limit"))
                .unwrap_or_else(|| "unknown".to_string());
            format!("; headroom: {room}")
        } else {
            String::new()
        };
        lines.push(format!(
            "Forecast peak: {resource} {value:.1} {unit}{percent} at +{}m{headroom}.",
            peak["minute"]
        ));
    }
    lines
}
