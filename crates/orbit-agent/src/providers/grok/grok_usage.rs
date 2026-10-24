//! Grok CLI billing API readings, independent of invocation token telemetry.

use chrono::{DateTime, Utc};
use orbit_types::telemetry::{ProviderLimitObservation, ProviderLimitSource};
use serde_json::Value;

/// Parse only the top-level billing response, never a headless answer's text.
pub(crate) fn grok_usage_windows(body: &[u8], now: DateTime<Utc>) -> Vec<ProviderLimitObservation> {
    let Some(reading) = reading(body, now) else {
        return Vec::new();
    };
    vec![reading]
}

fn reading(body: &[u8], now: DateTime<Utc>) -> Option<ProviderLimitObservation> {
    let response: Value = serde_json::from_slice(body).ok()?;
    if response.get("schemaVersion").is_some() {
        return None;
    }
    let config = response.get("config")?;
    let used = config.get("creditUsagePercent")?.as_f64()?;
    if !used.is_finite() || used < 0.0 {
        return None;
    }
    let period = config.get("currentPeriod")?;
    let start = DateTime::parse_from_rfc3339(period.get("start")?.as_str()?).ok()?;
    let end = DateTime::parse_from_rfc3339(period.get("end")?.as_str()?).ok()?;
    let seconds = (end - start).num_seconds();
    let minutes = u32::try_from(seconds / 60).ok().filter(|n| *n > 0)?;
    if seconds % 60 != 0 {
        return None;
    }
    let window = match period.get("type")?.as_str()? {
        "USAGE_PERIOD_TYPE_WEEKLY" => "weekly",
        // Unknown period types are advisory and must not gate an account.
        _ => return None,
    };
    Some(ProviderLimitObservation {
        provider: "grok".to_string(),
        account: None,
        model: None,
        window: Some(window.to_string()),
        exhausted: used >= 100.0,
        source: ProviderLimitSource::Event,
        resets_at: Some(end.with_timezone(&Utc)),
        used_percent: Some(used),
        window_minutes: Some(minutes),
        gating: true,
        partial: false,
        observed_at: now,
        run_id: None,
        crew: None,
        detail: "Grok CLI billing creditUsagePercent/currentPeriod".to_string(),
    })
}
