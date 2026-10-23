//! Render the recorded forecast without recalculating or reserving admission.

use chrono::{DateTime, Utc};
use serde_json::Value;

use super::format::format_duration;

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

/// The drain whose passes recorded a forecast, for labelling one that is
/// history rather than this pass's.
pub(super) struct ForecastOrigin<'a> {
    pub(super) run_id: Option<&'a str>,
    /// The drain has ended, so even its latest pass is history.
    pub(super) ended: bool,
    /// No drain, local or pull, is live in this workspace. `false` when the
    /// caller cannot tell.
    pub(super) none_live: bool,
}

pub(super) fn admission_lines(
    current: &Value,
    last_built: &Value,
    origin: &ForecastOrigin<'_>,
    mut title: impl FnMut(&str) -> Option<String>,
) -> Vec<String> {
    let mut lines = Vec::new();
    // A finished drain's last pass is retained history like an older pass's
    // forecast: an operator must not read its peaks as the host's load now.
    let current_has_forecast = !current["forecast_at"].is_null();
    let previous_forecast_retained = !current_has_forecast && last_built["forecast_at"].is_string();
    let current_retained = origin.ended && current_has_forecast;
    if let Some(line) = admission_line(current, &mut title) {
        if current_retained {
            lines.push(retained_line(current, &line, origin));
        } else if !previous_forecast_retained {
            lines.push(line);
        }
    }
    // This sentence claims admission is blocked now. Keep it only for a live
    // current pass, never for a terminal drain or a pass showing older history.
    if !origin.ended
        && !previous_forecast_retained
        && let Some(line) = stuck_head_line(current, &mut title)
    {
        lines.push(line);
    }
    let (forecast, retained) = if current_retained {
        (current, true)
    } else if current["forecast_at"].is_null() {
        if last_built["forecast_at"].is_string() {
            if lines.is_empty() {
                lines.push("Admission: no forecast built this pass".to_string());
            }
            if let Some(line) = admission_line(last_built, &mut title) {
                lines.push(retained_line(last_built, &line, origin));
            }
        }
        (last_built, true)
    } else {
        (current, false)
    };
    lines.extend(peak_lines(forecast, retained));
    lines
}

/// `Last forecast <at> (<age> ago, from drain <run>[, which has ended][; no
/// drain is live]): <decisions>`.
fn retained_line(forecast: &Value, line: &str, origin: &ForecastOrigin<'_>) -> String {
    let at = forecast["forecast_at"].as_str().unwrap_or("?");
    let mut context = Vec::new();
    if let Ok(recorded) = DateTime::parse_from_rfc3339(at) {
        let age = Utc::now().signed_duration_since(recorded.with_timezone(&Utc));
        let ms = u64::try_from(age.num_milliseconds()).unwrap_or(0);
        context.push(format!("{} ago", format_duration(Some(ms))));
    }
    if let Some(run_id) = origin.run_id {
        let ended = if origin.ended {
            ", which has ended"
        } else {
            ""
        };
        context.push(format!("from drain {run_id}{ended}"));
    }
    let mut context = context.join(", ");
    if origin.none_live {
        if !context.is_empty() {
            context.push_str("; ");
        }
        context.push_str("no drain is live");
    }
    let context = if context.is_empty() {
        String::new()
    } else {
        format!(" ({context})")
    };
    format!(
        "Last forecast {at}{context}: {}",
        line.trim_start_matches("Admission: ")
    )
}

/// `ORB-12 "title"`, the bare ID when its title is unreadable, or `fallback`
/// when the record names no task.
fn task_name(
    task_id: Option<&str>,
    title: &mut impl FnMut(&str) -> Option<String>,
    fallback: &str,
) -> String {
    match task_id {
        Some(id) => match title(id) {
            Some(title) => format!("{id} \"{title}\""),
            None => id.to_string(),
        },
        None => fallback.to_string(),
    }
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
            let name = task_name(admitted["task_id"].as_str(), &mut title, "task");
            decisions.push(format!("admitted {name}"));
        }
    }
    for skipped in forecast["skipped"].as_array().into_iter().flatten() {
        let name = task_name(skipped["task_id"].as_str(), &mut title, "task");
        decisions.push(format!("skipped {name} ({})", skip_detail(skipped)));
    }
    if forecast["head"]["protected"].as_bool() == Some(true) {
        let name = task_name(
            forecast["head"]["task_id"].as_str(),
            &mut title,
            "queue head",
        );
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
    let name = task_name(head["task_id"].as_str(), &mut title, "queue head");
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

// Derive the explanation from structured fields rather than the persisted
// detail, whose starvation wording would name the head a second time.
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

/// Peaks of a retained forecast are labelled `Last forecast peak:` so they
/// do not read as the host's load now.
pub(super) fn peak_lines(forecast: &Value, retained: bool) -> Vec<String> {
    let mut lines = Vec::new();
    for resource in ["cpu", "memory"] {
        let peak = &forecast[format!("{resource}_peak")];
        let Some(value) = peak["value"].as_f64() else {
            continue;
        };
        // Memory is recorded in bytes and shown in GiB, as `run profiles` does.
        let (value, unit) = if resource == "cpu" {
            (value, "cores")
        } else {
            (value / GIB, "GiB")
        };
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
        let label = if retained {
            "Last forecast peak"
        } else {
            "Forecast peak"
        };
        lines.push(format!(
            "{label}: {resource} {value:.1} {unit}{percent} at +{}m{headroom}.",
            peak["minute"]
        ));
    }
    lines
}
