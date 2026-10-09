//! [ORB-14695] A provider whose account hit a usage limit: the typed
//! `[provider_limit]` failure, and the observation the host records.
//! [ORB-14696] The usage windows a provider reported after any run.

use std::path::Path;

use chrono::{DateTime, Offset, Utc};
use orbit_agent::{provider_usage_limit, provider_usage_limit_details, provider_usage_windows};
use orbit_types::telemetry::{ProviderLimitObservation, ProviderLimitSource};
use orbit_types::workflow::ProviderLimitFailure;
use serde_json::Value;

use crate::context::RuntimeHost;

use super::completion::{provider_failure, stdout_frames};

/// Claude's terminal error `result` frame saying the account hit a usage
/// limit. Claude can write it with exit 0, so it fails the turn whatever the
/// exit code, like an authentication error.
pub(super) fn structured_provider_limit(provider: &str, stdout: &[u8]) -> Option<String> {
    if provider != "claude" {
        return None;
    }
    stdout_frames(stdout)
        .filter(|frame| frame.get("type").and_then(Value::as_str) == Some("result"))
        .filter_map(|frame| {
            provider_failure(provider, &frame)?
                .get("result")
                .and_then(Value::as_str)
                .filter(|text| provider_usage_limit(text))
                .map(str::to_string)
        })
        .last()
}

/// The limit a provider's own `text` and control frames describe, read at
/// `now` in the host's local time.
pub(super) fn reported_limit(
    provider: &str,
    text: &str,
    stdout: &[u8],
    now: DateTime<Utc>,
) -> ProviderLimitFailure {
    let local = chrono::Local::now().offset().fix();
    let mut limit = provider_usage_limit_details(text, now, local);
    if let Some((window, resets_at)) = claude_rate_limit(provider, stdout) {
        if limit.model.is_none() {
            limit.model = window
                .as_deref()
                .and_then(|window| window.strip_prefix("seven_day_"))
                .filter(|family| matches!(*family, "opus" | "sonnet"))
                .map(str::to_string);
        }
        limit.window = limit.window.or(window);
        limit.resets_at = limit.resets_at.or(resets_at);
    }
    // Whole seconds, as the failure text carries it, so the text, the hold
    // and the host's observation name one instant.
    limit.resets_at = limit
        .resets_at
        .and_then(|at| DateTime::from_timestamp(at.timestamp(), 0));
    limit
}

/// Claude's `rate_limit_info` with `status: "rejected"` on a control frame
/// (its `usage_limit_reached` error or a `rate_limit_event`): the window
/// that ran out and the epoch it resets at.
fn claude_rate_limit(
    provider: &str,
    stdout: &[u8],
) -> Option<(Option<String>, Option<DateTime<Utc>>)> {
    if provider != "claude" {
        return None;
    }
    stdout_frames(stdout)
        .filter(|frame| frame.get("schemaVersion").is_none())
        .filter_map(|frame| {
            let info = frame
                .get("rate_limit_info")
                .or_else(|| frame.get("error")?.get("rate_limit_info"))?
                .clone();
            (info.get("status").and_then(Value::as_str) == Some("rejected")).then_some(info)
        })
        .last()
        .map(|info| {
            let window = info
                .get("rateLimitType")
                .and_then(Value::as_str)
                .map(str::to_string);
            let resets_at = info
                .get("resetsAt")
                .and_then(Value::as_i64)
                .and_then(|epoch| DateTime::from_timestamp(epoch, 0));
            (window, resets_at)
        })
}

/// Record the limit in the host's provider-limit store. A host that cannot
/// record it still fails the step with the typed limit; the store is advisory.
pub(super) fn record_limit(
    host: &dyn RuntimeHost,
    provider: &str,
    limit: &ProviderLimitFailure,
    detail: &str,
    run_id: &str,
    crew: Option<&str>,
) {
    let observation = ProviderLimitObservation {
        provider: provider.to_string(),
        model: limit.model.clone(),
        window: limit.window.clone(),
        exhausted: true,
        source: ProviderLimitSource::Error,
        resets_at: limit.resets_at,
        observed_at: Utc::now(),
        run_id: (!run_id.is_empty()).then(|| run_id.to_string()),
        crew: crew.map(str::to_string),
        detail: ProviderLimitObservation::bounded_detail(detail),
        used_percent: None,
        window_minutes: None,
        gating: true,
        partial: false,
    };
    record(host, &observation);
}

/// [ORB-14696] Record each usage window the provider reported about its own
/// account during the run, success or failure. `stdout` is the whole capture,
/// since a long run's first frames sit before a truncation marker, and
/// `codex_home` the `CODEX_HOME` the Codex child ran with.
pub(super) fn record_usage_windows(
    host: &dyn RuntimeHost,
    provider: &str,
    stdout: &[u8],
    codex_home: Option<&Path>,
    run_id: &str,
    crew: Option<&str>,
) {
    for mut observation in provider_usage_windows(provider, stdout, codex_home, Utc::now()) {
        observation.run_id = (!run_id.is_empty()).then(|| run_id.to_string());
        observation.crew = crew.map(str::to_string);
        record(host, &observation);
    }
}

/// The store is advisory: a host that cannot record an observation logs it
/// and the step's outcome stands.
fn record(host: &dyn RuntimeHost, observation: &ProviderLimitObservation) {
    if let Err(error) = host.record_provider_limit(observation) {
        tracing::warn!(
            provider = observation.provider,
            run_id = observation.run_id,
            "could not record the provider usage limit on this host: {error}"
        );
    }
}
