//! A provider's own reading of its account's usage windows, taken after
//! every run. [ORB-14696]
//!
//! Codex writes windows to its session rollout under `CODEX_HOME`; Claude
//! reports them in stdout `rate_limit_event` frames, and Muse in stdout
//! `status` or `run.usage` records whose payload kind is `run_usage`. Grok's
//! windows come from its CLI billing API response; Antigravity's from its
//! `/usage` command. Other providers record no usage windows.

use std::path::Path;

use chrono::{DateTime, Utc};
use orbit_types::telemetry::ProviderLimitObservation;
use orbit_types::workflow::activity_job::Provider;
use serde_json::Value;

use super::antigravity::antigravity_usage_windows;
use super::claude::claude_usage_windows;
use super::codex::codex_usage_windows;
use super::grok::grok_usage_windows;
use super::muse::muse_usage_windows;

/// The usage windows `provider` reported about its own account during one
/// run, one observation per window with source `event`, read at `now`.
///
/// `stdout` is the run's capture, except for Grok and Antigravity, where the
/// caller supplies the CLI billing API or `/usage` command response instead.
/// `codex_home` is the `CODEX_HOME` the Codex
/// child ran with. Only frames the provider wrote at the top level of its
/// own output are read, never assistant text or tool output, and a reading
/// whose shape Orbit does not recognise records nothing. `run_id` and `crew`
/// are left for the caller.
#[must_use]
pub fn provider_usage_windows(
    provider: Provider,
    stdout: &[u8],
    codex_home: Option<&Path>,
    now: DateTime<Utc>,
) -> Vec<ProviderLimitObservation> {
    provider_usage_snapshot(provider, stdout, codex_home, now).observations
}

/// One provider reading: its windows and whether they are the account's whole
/// set. [ORB-26000]
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderUsageSnapshot {
    /// What [`provider_usage_windows`] returns.
    pub observations: Vec<ProviderLimitObservation>,
    /// Whether a window the reading lacks is gone from the account. `false`
    /// when a still-reported window could not be read, so the reading must not
    /// retire what an earlier one stored.
    pub complete: bool,
}

/// [`provider_usage_windows`] with whether the reading is complete. Only
/// Antigravity's `/usage` command names every window of the account, so every
/// other provider's reading is incomplete.
#[must_use]
pub fn provider_usage_snapshot(
    provider: Provider,
    stdout: &[u8],
    codex_home: Option<&Path>,
    now: DateTime<Utc>,
) -> ProviderUsageSnapshot {
    let incomplete = |observations| ProviderUsageSnapshot {
        observations,
        complete: false,
    };
    match provider {
        Provider::Claude => incomplete(claude_usage_windows(stdout, now)),
        Provider::Codex => incomplete(
            codex_home.map_or_else(Vec::new, |home| codex_usage_windows(stdout, home, now)),
        ),
        Provider::Muse => incomplete(muse_usage_windows(stdout, now)),
        Provider::Grok => incomplete(grok_usage_windows(stdout, now)),
        Provider::Antigravity => {
            let (observations, complete) = antigravity_usage_windows(stdout, now);
            ProviderUsageSnapshot {
                observations,
                complete,
            }
        }
        Provider::Gemini
        | Provider::Copilot
        | Provider::Ollama
        | Provider::OpenaiCompat
        | Provider::Cursor
        | Provider::Pi
        | Provider::Opencode => incomplete(Vec::new()),
    }
}

/// Each line of a JSONL capture that is a provider frame: a JSON object that
/// is not an Orbit envelope, with the line it came from. A line that is not
/// JSON, such as Orbit's capture-truncation marker, is skipped.
pub(super) fn top_level_frames(stdout: &[u8]) -> impl Iterator<Item = (&[u8], Value)> {
    stdout.split(|byte| *byte == b'\n').filter_map(|line| {
        let line = line.trim_ascii();
        let frame = serde_json::from_slice::<Value>(line).ok()?;
        (frame.is_object() && frame.get("schemaVersion").is_none()).then_some((line, frame))
    })
}

/// A percentage rounded to hundredths, so a fraction such as `0.91` reads
/// back as `91`.
pub(super) fn percent(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}
