//! Structured quota buckets from agy 1.4.6's `--print /usage --output-format json`.

use chrono::{DateTime, Utc};
use orbit_types::telemetry::{ProviderLimitObservation, ProviderLimitSource};
use serde_json::Value;

use super::super::usage_window::percent;

/// Read provider control data only; the human-readable `response` is ignored.
pub(crate) fn antigravity_usage_windows(
    body: &[u8],
    now: DateTime<Utc>,
) -> Vec<ProviderLimitObservation> {
    let Some(buckets) = buckets(body) else {
        return Vec::new();
    };
    // Pro accounts expose a separate Argon bucket. Never apply the Flash/Pro
    // quota to Argon there. Ultra includes Argon in the shared Gemini bucket.
    let separate_argon = buckets
        .iter()
        .any(|bucket| bucket.get("id").and_then(Value::as_str) == Some("gemini-argon-wtu-weekly"));
    let mut observations = Vec::new();
    for bucket in buckets {
        let Some((id, scopes, minutes, used, reset)) = reading(&bucket, separate_argon) else {
            continue;
        };
        // The store scopes models by substring. Split shared groups into
        // their covered families so a third-party quota cannot gate Gemini.
        for scope in scopes {
            observations.push(ProviderLimitObservation {
                provider: "antigravity".to_string(),
                account: None,
                model: Some((*scope).to_string()),
                window: Some(id.to_string()),
                exhausted: used >= 100.0,
                source: ProviderLimitSource::Event,
                resets_at: Some(reset),
                used_percent: Some(used),
                window_minutes: Some(minutes),
                gating: true,
                partial: false,
                observed_at: now,
                run_id: None,
                crew: None,
                detail: "Antigravity CLI /usage command.data.groups[].buckets[]".to_string(),
            });
        }
    }
    observations
}

fn buckets(body: &[u8]) -> Option<Vec<Value>> {
    let response: Value = serde_json::from_slice(body).ok()?;
    if response.get("schemaVersion").is_some()
        || response.get("status")?.as_str()? != "SUCCESS"
        || response.pointer("/command/name")?.as_str()? != "usage"
    {
        return None;
    }
    Some(
        response
            .pointer("/command/data/groups")?
            .as_array()?
            .iter()
            .filter_map(|group| group.get("buckets")?.as_array())
            .flatten()
            .cloned()
            .collect(),
    )
}

type Reading<'a> = (&'a str, &'static [&'static str], u32, f64, DateTime<Utc>);

fn reading(bucket: &Value, separate_argon: bool) -> Option<Reading<'_>> {
    if bucket
        .get("disabled")
        .is_some_and(|disabled| disabled != false)
    {
        return None;
    }
    let id = bucket.get("id")?.as_str()?;
    let window = bucket.get("window")?.as_str()?;
    let (scopes, minutes): (&[&str], u32) = match (id, window) {
        ("gemini-weekly", "weekly") => (
            if separate_argon {
                &["flash", "pro"]
            } else {
                &["gemini"]
            },
            10080,
        ),
        ("gemini-5h", "5h") => (
            if separate_argon {
                &["flash", "pro"]
            } else {
                &["gemini"]
            },
            300,
        ),
        ("3p-weekly", "weekly") => (&["claude", "gpt"], 10080),
        ("3p-5h", "5h") => (&["claude", "gpt"], 300),
        ("gemini-argon-wtu-weekly", "weekly") => (&["argon"], 10080),
        _ => return None,
    };
    let remaining = bucket.get("remaining_fraction")?.as_f64()?;
    if !remaining.is_finite() || !(0.0..=1.0).contains(&remaining) {
        return None;
    }
    let reset = DateTime::parse_from_rfc3339(bucket.get("reset_time")?.as_str()?).ok()?;
    Some((
        id,
        scopes,
        minutes,
        percent((1.0 - remaining) * 100.0),
        reset.with_timezone(&Utc),
    ))
}
