//! Sweep filing: turning external evidence into tasks.
//!
//! `ci_failure` files CI failures as proposed repair tasks and admits piloted
//! repairs to the backlog; `dependabot` files Dependabot, code scanning and
//! secret scanning alerts. `duplicate_tasks` decides whether an existing task
//! already owns a finding. This module holds the helpers both sweeps share.

pub(crate) mod ci_failure;
pub(crate) mod dependabot;
mod duplicate_tasks;
mod rule_evidence;
pub(crate) mod rule_findings;
mod task_port;

use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_all;
use orbit_types::identity::lower_hex;
use orbit_types::task::{TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::LandingObservationStatus;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::OrbitRuntime;

/// Hex characters of a signature digest kept in a tag. Full-width digests
/// make a tag unreadable in a task list; this is a dedupe key, not a security
/// boundary.
const KEY_LEN: usize = 16;

/// Short stable key over `parts`, NUL-separated so part boundaries matter.
pub(crate) fn digest(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update([0u8]);
    }
    lower_hex(hasher.finalize()).chars().take(KEY_LEN).collect()
}

/// Read optional positive integer `input.<key>` (number or numeric string),
/// defaulting when absent and capping at `max`.
pub(crate) fn bounded_u64(
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

pub(crate) fn display(value: &str) -> &str {
    if value.is_empty() { "unknown" } else { value }
}

pub(crate) fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    value.chars().take(max_chars).collect::<String>() + "…"
}

/// Redact and bound an error message included in a sweep result.
pub(crate) fn bounded_error(message: &str) -> String {
    redact_all(message).chars().take(500).collect()
}

/// Whether `value` is a full 40-character hexadecimal revision.
pub(crate) fn full_revision(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Prefix of the run-store error when this workspace has no delivery run for
/// the task. `latest_delivery_run_id` in the delivery observation builds it.
const ABSENT_DELIVERY_RUN_PREFIX: &str =
    "no task delivery run in this workspace was submitted with task ";

/// Read the merged landed commit for `task_id`, if delivery evidence is valid.
///
/// An owner-run delivery is read from this workspace's run store. A pull
/// follower's run is not in that store, so a missing run falls back to the
/// owner's `landing_completed` note. A present run, merged or not, stays on
/// the run-store result.
pub(crate) fn landed_task_commit(runtime: &OrbitRuntime, task_id: &str) -> Option<String> {
    match runtime.observe_task_delivery(task_id, None) {
        Ok(observation) => {
            return (observation.landing.status == LandingObservationStatus::Merged)
                .then_some(observation.landing.landed_commit)
                .flatten()
                .filter(|commit| full_revision(commit));
        }
        Err(error) if absent_delivery_run(&error) => {}
        Err(_) => return None,
    }
    let history = runtime.get_task_history(task_id).ok()?;
    owner_landing_commit(&history).filter(|commit| full_revision(commit))
}

/// Whether `error` is the run store reporting that no delivery run for the
/// task exists in this workspace.
fn absent_delivery_run(error: &OrbitError) -> bool {
    matches!(
        error,
        OrbitError::InvalidInput(message) if message.starts_with(ABSENT_DELIVERY_RUN_PREFIX)
    )
}

/// Merge commit on the latest owner `landing_completed` note.
///
/// Pull-request evidence is `delivery.merge_commit`. A local landing records
/// `delivery` as `local_candidate`, or `reconciled` as
/// `local_target_contains_candidate`, and the commit in `candidate`. The
/// codebase landing receipt reads the same note; the sweep only needs the
/// commit.
fn owner_landing_commit(history: &[TaskHistoryEntry]) -> Option<String> {
    history.iter().rev().find_map(|entry| {
        if entry.event != "landing_completed" {
            return None;
        }
        let note: Value = serde_json::from_str(entry.note.as_deref()?).ok()?;
        let commit = note
            .pointer("/delivery/merge_commit")
            .and_then(Value::as_str)
            .or_else(|| {
                (note["delivery"] == "local_candidate"
                    || note["reconciled"] == "local_target_contains_candidate")
                    .then(|| note["candidate"].as_str())
                    .flatten()
            })?;
        Some(commit.to_string())
    })
}

/// Whether a task status can still own an open CI-failure sweep finding.
pub(crate) fn is_open_ci_owner_status(status: TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Proposed
            | TaskStatus::Backlog
            | TaskStatus::InProgress
            | TaskStatus::Review
            | TaskStatus::Blocked
    )
}
