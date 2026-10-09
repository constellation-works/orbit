//! A provider's own reading of its account's usage windows, taken after
//! every run. [ORB-14696]
//!
//! Only Codex and Claude report one. Codex writes it to the session rollout
//! under `CODEX_HOME`; Claude writes it to its message stream on stdout.
//! Other providers' output formats carry no usage window, so they record
//! nothing.

use std::path::Path;

use chrono::{DateTime, Utc};
use orbit_types::telemetry::ProviderLimitObservation;
use serde_json::Value;

use super::claude::claude_usage_windows;
use super::codex::codex_usage_windows;

/// The usage windows `provider` reported about its own account during one
/// run, one observation per window with source `event`, read at `now`.
///
/// `stdout` is the run's capture. `codex_home` is the `CODEX_HOME` the Codex
/// child ran with. Only frames the provider wrote at the top level of its
/// own output are read, never assistant text or tool output, and a reading
/// whose shape Orbit does not recognise records nothing. `run_id` and `crew`
/// are left for the caller.
#[must_use]
pub fn provider_usage_windows(
    provider: &str,
    stdout: &[u8],
    codex_home: Option<&Path>,
    now: DateTime<Utc>,
) -> Vec<ProviderLimitObservation> {
    match provider {
        "claude" => claude_usage_windows(stdout, now),
        "codex" => codex_home.map_or_else(Vec::new, |home| codex_usage_windows(stdout, home, now)),
        _ => Vec::new(),
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
