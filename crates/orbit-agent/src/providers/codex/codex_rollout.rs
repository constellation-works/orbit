//! Codex's own usage-window reading, from the run's session rollout.
//! [ORB-14696]
//!
//! `codex exec --json` stdout carries no rate limits. Codex writes them to the
//! session rollout it keeps for every non-ephemeral exec, at
//! `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-<timestamp>-<thread_id>.jsonl`,
//! where the date is the host's local date when the session started. Each
//! `event_msg` whose payload `type` is `token_count` carries
//! `rate_limits {limit_id, primary, secondary, plan_type, …}`, and each
//! window is `{used_percent, window_minutes, resets_at (epoch s)}`
//! (codex-cli 0.161.0).
//!
//! The same `token_count` payload's `info.last_token_usage` is the usage of
//! the one model request that just finished, which prompt-size price tiers
//! need; stdout's `turn.completed` only totals the turn.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Local, NaiveDate, Utc};
use orbit_types::telemetry::{ProviderLimitObservation, ProviderLimitSource, TokenUsage};
use serde_json::{Map, Value};

use super::super::usage_window::{percent, top_level_frames};
use crate::types::request_usage;

/// The most of a rollout's end Orbit reads. A long session's rollout runs to
/// megabytes, and its last `token_count` is near the end.
const ROLLOUT_TAIL_BYTES: u64 = 1024 * 1024;

/// The thread the run's own `thread.started` frame names. Frames that name
/// two threads name none.
fn run_thread_id(stdout: &[u8]) -> Option<String> {
    let mut thread_ids = top_level_frames(stdout)
        .filter(|(_, frame)| frame.get("type").and_then(Value::as_str) == Some("thread.started"))
        .filter_map(|(_, frame)| Some(frame.get("thread_id")?.as_str()?.to_string()));
    let thread_id = thread_ids.next()?;
    let safe = !thread_id.is_empty()
        && thread_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
    (safe && thread_ids.all(|other| other == thread_id)).then_some(thread_id)
}

/// The rollout of `thread_id` under `codex_home`. Its date directory is the
/// local date the session started, so look on the days around `now`, in
/// local time and in UTC.
fn rollout_path(codex_home: &Path, thread_id: &str, now: DateTime<Utc>) -> Option<PathBuf> {
    let suffix = format!("-{thread_id}.jsonl");
    let mut dates: Vec<NaiveDate> = [now, now - Duration::days(1)]
        .into_iter()
        .flat_map(|at| [at.with_timezone(&Local).date_naive(), at.date_naive()])
        .collect();
    dates.sort_unstable();
    dates.dedup();
    dates.into_iter().rev().find_map(|date| {
        let dir = codex_home
            .join("sessions")
            .join(date.format("%Y").to_string())
            .join(date.format("%m").to_string())
            .join(date.format("%d").to_string());
        std::fs::read_dir(dir).ok()?.flatten().find_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?;
            (name.starts_with("rollout-") && name.ends_with(&suffix)).then(|| entry.path())
        })
    })
}

/// The last [`ROLLOUT_TAIL_BYTES`] of `path`, from its first complete line.
fn rollout_tail(path: &Path) -> Option<Vec<u8>> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(ROLLOUT_TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut tail = Vec::new();
    file.take(ROLLOUT_TAIL_BYTES).read_to_end(&mut tail).ok()?;
    if start > 0 {
        let first_line = tail.iter().position(|byte| *byte == b'\n')?;
        tail.drain(..=first_line);
    }
    Some(tail)
}

/// The usage windows the run's rollout last reported, one observation per
/// window. A missing rollout, another thread's rollout, or a `token_count`
/// with no `rate_limits` records nothing.
pub(crate) fn codex_usage_windows(
    stdout: &[u8],
    codex_home: &Path,
    now: DateTime<Utc>,
) -> Vec<ProviderLimitObservation> {
    let Some(thread_id) = run_thread_id(stdout) else {
        return Vec::new();
    };
    let Some(tail) = rollout_path(codex_home, &thread_id, now).and_then(|path| rollout_tail(&path))
    else {
        return Vec::new();
    };
    let Some((timestamp, limits)) = top_level_frames(&tail)
        .filter(|(_, frame)| frame.get("type").and_then(Value::as_str) == Some("event_msg"))
        .filter_map(|(_, frame)| {
            let payload = frame.get("payload")?;
            if payload.get("type").and_then(Value::as_str) != Some("token_count") {
                return None;
            }
            let limits = payload.get("rate_limits")?.as_object()?.clone();
            let timestamp = frame
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
                .map(|at| at.with_timezone(&Utc));
            Some((timestamp, limits))
        })
        .last()
    else {
        return Vec::new();
    };
    let detail = ["limit_id", "plan_type"]
        .iter()
        .filter_map(|key| Some(format!("{key}={}", limits.get(*key)?.as_str()?)))
        .collect::<Vec<_>>()
        .join(" ");
    let reading = |window: &str, fields: &Map<String, Value>| {
        let used_percent = percent(fields.get("used_percent")?.as_f64()?);
        Some(ProviderLimitObservation {
            provider: "codex".to_string(),
            account: None,
            // Codex's windows are account-wide.
            model: None,
            window: Some(window.to_string()),
            exhausted: used_percent >= 100.0,
            source: ProviderLimitSource::Event,
            resets_at: fields
                .get("resets_at")
                .and_then(Value::as_i64)
                .and_then(|epoch| DateTime::from_timestamp(epoch, 0)),
            observed_at: timestamp.unwrap_or(now),
            run_id: None,
            crew: None,
            detail: ProviderLimitObservation::bounded_detail(&detail),
            used_percent: Some(used_percent),
            window_minutes: fields
                .get("window_minutes")
                .and_then(Value::as_u64)
                .and_then(|minutes| u32::try_from(minutes).ok()),
            gating: true,
            partial: false,
        })
    };
    ["primary", "secondary"]
        .into_iter()
        .filter_map(|window| reading(window, limits.get(window)?.as_object()?))
        .collect()
}

/// The usage of each model request in the run's rollout whose prompt may
/// reach a prompt-size price tier, or `None` when the rollout cannot be found
/// or read.
///
/// The whole rollout is scanned, since a long session's first requests are
/// far from its end. Codex can repeat a `token_count` without a new request
/// (a rate-limit refresh); its `total_token_usage` has not moved, so it is
/// skipped.
pub(crate) fn codex_long_prompt_requests(
    stdout: &[u8],
    codex_home: &Path,
    now: DateTime<Utc>,
) -> Option<Vec<TokenUsage>> {
    let thread_id = run_thread_id(stdout)?;
    let path = rollout_path(codex_home, &thread_id, now)?;
    let reader = BufReader::new(File::open(path).ok()?);
    let mut requests = Vec::new();
    let mut last_total: Option<Value> = None;
    for line in reader.split(b'\n') {
        let line = line.ok()?;
        // Only `token_count` events matter; skip parsing every other frame.
        if !line
            .windows(b"token_count".len())
            .any(|w| w == b"token_count")
        {
            continue;
        }
        let Ok(frame) = serde_json::from_slice::<Value>(line.trim_ascii()) else {
            continue;
        };
        if frame.get("type").and_then(Value::as_str) != Some("event_msg") {
            continue;
        }
        let Some(payload) = frame.get("payload") else {
            continue;
        };
        if payload.get("type").and_then(Value::as_str) != Some("token_count") {
            continue;
        }
        let Some(info) = payload.get("info").and_then(Value::as_object) else {
            continue;
        };
        let total = info.get("total_token_usage").cloned();
        if total.is_some() && total == last_total {
            continue;
        }
        last_total = total;
        if let Some(usage) = info
            .get("last_token_usage")
            .and_then(Value::as_object)
            .and_then(request_usage)
            .filter(TokenUsage::may_reach_long_prompt_floor)
        {
            requests.push(usage);
        }
    }
    Some(requests)
}
