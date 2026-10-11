//! Helpers shared by the sweep actions that file tasks from external evidence
//! (`file_ci_failure_tasks`, `file_dependabot_alert_tasks`).

use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_all;
use orbit_types::identity::lower_hex;
use orbit_types::task::TaskStatus;
use orbit_types::workflow::LandingObservationStatus;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::OrbitRuntime;

/// Hex characters of a signature digest kept in a tag. Full-width digests
/// make a tag unreadable in a task list; this is a dedupe key, not a security
/// boundary.
const KEY_LEN: usize = 16;

/// Short stable key over `parts`, NUL-separated so part boundaries matter.
pub(in crate::adapter::engine_host::v2_host) fn digest(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update([0u8]);
    }
    lower_hex(hasher.finalize()).chars().take(KEY_LEN).collect()
}

/// Read optional positive integer `input.<key>` (number or numeric string),
/// defaulting when absent and capping at `max`.
pub(in crate::adapter::engine_host::v2_host) fn bounded_u64(
    input: &Value,
    key: &str,
    default: u64,
    max: u64,
) -> Result<u64, OrbitError> {
    let Some(value) = input.get(key).filter(|value| !value.is_null()) else {
        return Ok(default);
    };
    let raw = match value {
        Value::Number(number) => number.as_u64(),
        Value::String(text) => text.trim().parse::<u64>().ok(),
        _ => None,
    }
    .ok_or_else(|| OrbitError::InvalidInput(format!("input.{key} must be a positive integer")))?;
    if raw == 0 {
        return Err(OrbitError::InvalidInput(format!(
            "input.{key} must be greater than zero"
        )));
    }
    Ok(raw.min(max))
}

pub(in crate::adapter::engine_host::v2_host) fn display(value: &str) -> &str {
    if value.is_empty() { "unknown" } else { value }
}

pub(in crate::adapter::engine_host::v2_host) fn truncate_chars(
    value: &str,
    max_chars: usize,
) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    value.chars().take(max_chars).collect::<String>() + "…"
}

/// Redact and bound an error message included in a sweep result.
pub(in crate::adapter::engine_host::v2_host) fn bounded_error(message: &str) -> String {
    redact_all(message).chars().take(500).collect()
}

/// Whether `value` is a full 40-character hexadecimal revision.
pub(in crate::adapter::engine_host::v2_host) fn full_revision(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Read the merged landed commit for `task_id`, if delivery evidence is valid.
pub(in crate::adapter::engine_host::v2_host) fn landed_task_commit(
    runtime: &OrbitRuntime,
    task_id: &str,
) -> Option<String> {
    let observation = runtime.observe_task_delivery(task_id, None).ok()?;
    (observation.landing.status == LandingObservationStatus::Merged)
        .then_some(observation.landing.landed_commit)
        .flatten()
        .filter(|commit| full_revision(commit))
}

/// Whether a task status can still own an open CI-failure sweep finding.
pub(in crate::adapter::engine_host::v2_host) fn is_open_ci_owner_status(
    status: TaskStatus,
) -> bool {
    matches!(
        status,
        TaskStatus::Proposed
            | TaskStatus::Backlog
            | TaskStatus::InProgress
            | TaskStatus::Review
            | TaskStatus::Blocked
    )
}
