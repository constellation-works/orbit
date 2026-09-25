//! Pure projections of a run's already-read v2 audit trail: steps, provider
//! processes, recovery attempts, and invocation blob previews.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use orbit_common::process::identity::ProcessLiveness;
use orbit_common::security::redaction::redact_all;
use orbit_common::storage::blob_store::BlobStore;
use serde_json::Value;

use super::run::{
    MAX_RECOVERY_ATTEMPTS, RunAuditEvent, RunAuditStep, RunProviderProcess, RunRecoveryAttempt,
    RunRecoveryAttempts,
};

const MAX_RECOVERY_DIAGNOSTIC_CHARS: usize = 1024;

pub(super) fn latest_timestamp_from_envelope_rows(
    rows: impl IntoIterator<Item = orbit_store::V2AuditEventRow>,
) -> Option<DateTime<Utc>> {
    rows.into_iter()
        .filter_map(|row| serde_json::from_str::<Value>(&row.payload_json).ok())
        // Match the full audit projection's envelope validity boundary without
        // reconstructing parent links or activity steps just to read `ts`.
        .filter(|value| value.get("event_id").and_then(Value::as_str).is_some())
        .filter_map(|value| {
            value
                .get("ts")
                .and_then(Value::as_str)
                .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
                .map(|value| value.with_timezone(&Utc))
        })
        .max()
}

/// Reconstruct a run's activity steps from an already-read audit trail, in
/// first-started order.
pub(super) fn audit_steps_from_events(events: &[RunAuditEvent]) -> Vec<RunAuditStep> {
    let mut steps = Vec::<RunAuditStep>::new();
    let mut index_by_id = HashMap::<String, usize>::new();

    for event in events {
        match event.body_kind.as_deref() {
            Some("step_started") => {
                let Some(step_id) = event.raw.get("step_id").and_then(Value::as_str) else {
                    continue;
                };
                if index_by_id.contains_key(step_id) {
                    continue;
                }
                let index = steps.len();
                index_by_id.insert(step_id.to_string(), index);
                steps.push(RunAuditStep {
                    step_index: index as u32,
                    step_id: step_id.to_string(),
                    started_at: event.timestamp,
                    finished_at: None,
                    state: None,
                    outcome: None,
                    error_message: None,
                });
            }
            Some("step_finished") | Some("step_skipped") | Some("step_denied") => {
                let Some(step_id) = event.raw.get("step_id").and_then(Value::as_str) else {
                    continue;
                };
                let Some(index) = index_by_id.get(step_id).copied() else {
                    continue;
                };
                let step = &mut steps[index];
                step.finished_at = event.timestamp;
                match event.body_kind.as_deref() {
                    Some("step_finished") => {
                        let outcome = event
                            .raw
                            .get("outcome")
                            .and_then(Value::as_str)
                            .unwrap_or("finished")
                            .to_string();
                        step.state = Some(outcome.clone());
                        step.outcome = Some(outcome);
                        step.error_message = event
                            .raw
                            .get("error_message")
                            .and_then(Value::as_str)
                            .map(str::to_string);
                    }
                    Some("step_skipped") => {
                        step.state = Some("skipped".to_string());
                        step.outcome = Some("skipped".to_string());
                        step.error_message = event
                            .raw
                            .get("reason")
                            .and_then(Value::as_str)
                            .map(str::to_string);
                    }
                    Some("step_denied") => {
                        step.state = Some("failed".to_string());
                        step.outcome = Some("denied".to_string());
                        step.error_message = event
                            .raw
                            .get("reason")
                            .and_then(Value::as_str)
                            .map(str::to_string);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    steps
}

/// Index each activity step by id so a provider process can name its position
/// in the run as well as its step.
pub(super) fn step_index_by_id(steps: &[RunAuditStep]) -> HashMap<String, u32> {
    steps
        .iter()
        .map(|step| (step.step_id.clone(), step.step_index))
        .collect()
}

/// Reconstruct the run's provider subprocesses from an already-read audit
/// trail, pairing each spawn with the completion that closes it and probing the
/// liveness of whatever is still open.
pub(super) fn provider_processes_from_events<P>(
    run_id: &str,
    events: Vec<RunAuditEvent>,
    step_index_by_id: &HashMap<String, u32>,
    probe: P,
) -> Vec<RunProviderProcess>
where
    P: Fn(u32, Option<&str>) -> ProcessLiveness,
{
    let mut records: Vec<RunProviderProcess> = Vec::new();
    // The direct parent of a provider process / completion event is the
    // invocation that emitted it. Keep that correlation private to this
    // reconstruction rather than projecting it as a new API field.
    let mut invocation_parent_by_process_event = HashMap::<String, String>::new();

    for event in events {
        match event.body_kind.as_deref() {
            Some("cli_invocation_process") => {
                let Some(pid) = event
                    .raw
                    .get("pid")
                    .and_then(Value::as_u64)
                    .and_then(|pid| u32::try_from(pid).ok())
                else {
                    continue;
                };
                let step_index = event
                    .step_id
                    .as_ref()
                    .and_then(|step_id| step_index_by_id.get(step_id).copied());
                if let Some(parent_event_id) = &event.parent_event_id {
                    invocation_parent_by_process_event
                        .insert(event.event_id.clone(), parent_event_id.clone());
                }
                records.push(RunProviderProcess {
                    run_id: event
                        .raw
                        .get("run_id")
                        .and_then(Value::as_str)
                        .unwrap_or(run_id)
                        .to_string(),
                    event_id: event.event_id,
                    ts: event.timestamp,
                    step_index,
                    step_id: event.step_id,
                    provider: event
                        .raw
                        .get("provider")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    pid,
                    pid_start_time: event
                        .raw
                        .get("pid_start_time")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    finished: false,
                    exit_code: None,
                    timed_out: false,
                    duration_ms: None,
                    // Overwritten below; only unfinished records are probed.
                    liveness: ProcessLiveness::Exited,
                });
            }
            Some("cli_invocation_finished") => {
                let Some(record) = matching_provider_process_for_completion(
                    &mut records,
                    &invocation_parent_by_process_event,
                    &event,
                ) else {
                    continue;
                };
                record.finished = true;
                record.exit_code = event.raw.get("exit_code").and_then(Value::as_i64);
                record.timed_out = event
                    .raw
                    .get("timed_out")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                record.duration_ms = event.raw.get("duration_ms").and_then(Value::as_u64);
            }
            _ => {}
        }
    }

    for record in &mut records {
        if !record.finished {
            record.liveness = probe(record.pid, record.pid_start_time.as_deref());
        }
    }

    records
}

/// Keep the newest `limit` provider children, preferring the ones still open.
///
/// A run with a long retry history can spawn more children than the budget
/// carries. Dropping an open invocation would hide exactly the child this
/// projection exists to report, so open records claim the budget first and the
/// newest finished ones fill what is left. Survivors stay in trail order.
pub(super) fn bound_provider_processes(
    records: Vec<RunProviderProcess>,
    limit: usize,
) -> (Vec<RunProviderProcess>, bool) {
    if records.len() <= limit {
        return (records, false);
    }

    let mut keep = vec![false; records.len()];
    let mut budget = limit;
    for keeping_open in [true, false] {
        for (index, record) in records.iter().enumerate().rev() {
            if budget == 0 {
                break;
            }
            if record.finished == keeping_open {
                continue;
            }
            keep[index] = true;
            budget -= 1;
        }
    }

    let kept = records
        .into_iter()
        .zip(keep)
        .filter_map(|(record, keep)| keep.then_some(record))
        .collect::<Vec<_>>();
    // Only reachable past the early return above, so something was dropped.
    (kept, true)
}

/// Find the open provider process that a completion can honestly close.
///
/// Modern events carry their emitting invocation as `parent_event_id`, so a
/// completion must match that identity as well as its enclosing step. Older
/// traces can lack ancestry. In that case a sole ancestry-free open process is
/// unambiguous (including sequential retries); multiple candidates remain open
/// rather than guessing which concurrent invocation completed.
fn matching_provider_process_for_completion<'a>(
    records: &'a mut [RunProviderProcess],
    invocation_parent_by_process_event: &HashMap<String, String>,
    completion: &RunAuditEvent,
) -> Option<&'a mut RunProviderProcess> {
    let mut candidates = records
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, record)| !record.finished && record.step_id == completion.step_id)
        .map(|(index, _)| index);

    let index = match completion.parent_event_id.as_deref() {
        Some(parent_event_id) => candidates.find(|index| {
            invocation_parent_by_process_event
                .get(&records[*index].event_id)
                .is_some_and(|record_parent| record_parent == parent_event_id)
        }),
        None => {
            let index = candidates.find(|index| {
                !invocation_parent_by_process_event.contains_key(&records[*index].event_id)
            })?;
            if candidates.any(|index| {
                !invocation_parent_by_process_event.contains_key(&records[index].event_id)
            }) {
                None
            } else {
                Some(index)
            }
        }
    }?;

    records.get_mut(index)
}

pub(super) fn enclosing_step_id(event: &Value, events: &HashMap<String, Value>) -> Option<String> {
    if let Some(step_id) = event.get("step_id").and_then(Value::as_str) {
        return Some(step_id.to_string());
    }

    let mut parent_id = event
        .get("parent_event_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let mut seen = HashSet::new();
    while let Some(id) = parent_id {
        if !seen.insert(id.clone()) {
            return None;
        }
        let parent = events.get(&id)?;
        if parent.get("body_kind").and_then(Value::as_str) == Some("step_started") {
            return parent
                .get("step_id")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        parent_id = parent
            .get("parent_event_id")
            .and_then(Value::as_str)
            .map(str::to_string);
    }
    None
}

pub(super) fn recovery_attempts_from_partitioned_rows(
    run_id: &str,
    rows: &[orbit_store::V2AuditEventRow],
) -> RunRecoveryAttempts {
    let truncated = rows.len() > MAX_RECOVERY_ATTEMPTS;
    let mut attempts = rows
        .iter()
        .filter_map(recovery_event_from_row)
        .filter_map(|event| recovery_attempt_from_event(run_id, event))
        .take(MAX_RECOVERY_ATTEMPTS)
        .collect::<Vec<_>>();
    attempts.reverse();
    RunRecoveryAttempts {
        state: if attempts.is_empty() {
            "not_attempted"
        } else {
            "recorded"
        },
        attempts,
        limit: MAX_RECOVERY_ATTEMPTS,
        truncated,
    }
}

fn recovery_event_from_row(row: &orbit_store::V2AuditEventRow) -> Option<RunAuditEvent> {
    let raw: Value = serde_json::from_str(&row.payload_json).ok()?;
    let event_id = raw.get("event_id").and_then(Value::as_str)?.to_string();
    Some(RunAuditEvent {
        parent_event_id: raw
            .get("parent_event_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        event_type: raw
            .get("event_type")
            .and_then(Value::as_str)
            .map(str::to_string),
        body_kind: raw
            .get("body_kind")
            .and_then(Value::as_str)
            .map(str::to_string),
        timestamp: raw
            .get("ts")
            .and_then(Value::as_str)
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.with_timezone(&Utc))
            .or(Some(row.ts)),
        step_id: raw
            .get("step_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        raw,
        event_id,
    })
}

fn recovery_attempt_from_event(run_id: &str, event: RunAuditEvent) -> Option<RunRecoveryAttempt> {
    let failed_step_id = event.raw.get("step_id")?.as_str()?.to_string();
    let recovery_activity = event.raw.get("recovery_activity")?.as_str()?.to_string();
    let recovery_succeeded = event.raw.get("recovery_succeeded")?.as_bool()?;
    let (diagnostic, diagnostic_truncated) = event
        .raw
        .get("error_message")
        .and_then(Value::as_str)
        .map(bounded_recovery_diagnostic)
        .map_or((None, false), |(diagnostic, truncated)| {
            (Some(diagnostic), truncated)
        });

    Some(RunRecoveryAttempt {
        run_id: event
            .raw
            .get("run_id")
            .and_then(Value::as_str)
            .unwrap_or(run_id)
            .to_string(),
        event_id: event.event_id,
        attempted_at: event.timestamp,
        failed_step_id,
        recovery_activity,
        outcome: if recovery_succeeded {
            "succeeded".to_string()
        } else {
            "failed".to_string()
        },
        failure_phase: event
            .raw
            .get("failure_phase")
            .and_then(Value::as_str)
            .map(str::to_string),
        diagnostic,
        diagnostic_truncated,
    })
}

fn bounded_recovery_diagnostic(raw: &str) -> (String, bool) {
    let redacted = redact_all(raw);
    let mut bounded = redacted
        .chars()
        .take(MAX_RECOVERY_DIAGNOSTIC_CHARS)
        .collect::<String>();
    let truncated = bounded.chars().count() < redacted.chars().count();
    if truncated {
        bounded.push('…');
    }
    (bounded, truncated)
}

fn read_blob_text_best_effort(blob_store: &BlobStore, blob_ref: &str) -> String {
    blob_store
        .read(blob_ref)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default()
}

/// Returns the blob text and whether the blob has bytes beyond what was
/// returned (only meaningful in preview mode; a full read is never
/// truncated here — the caller's own line-budget check covers it).
pub(super) fn read_invocation_blob(
    blob_store: &BlobStore,
    blob_ref: Option<&str>,
    preview_max_bytes: Option<usize>,
) -> (String, bool) {
    let Some(blob_ref) = blob_ref else {
        return (String::new(), false);
    };
    match preview_max_bytes {
        Some(max_bytes) => read_blob_text_preview_best_effort(blob_store, blob_ref, max_bytes),
        None => (read_blob_text_best_effort(blob_store, blob_ref), false),
    }
}

/// Read the preview window plus the remainder of the current line so
/// line-oriented truncation matches a full-blob read, without loading a
/// multi-MB agent transcript. The extra line is itself capped at
/// `max_bytes` so a newline-free blob is still not fully loaded.
///
/// The returned bool reports whether the blob extends past the returned
/// window. Without it, a blob whose byte at `max_bytes` is `\n` returns
/// exactly the first line and looks complete to a caller that only sees the
/// text — even though gigabytes may follow (DANI-10505).
fn read_blob_text_preview_best_effort(
    blob_store: &BlobStore,
    blob_ref: &str,
    max_bytes: usize,
) -> (String, bool) {
    let cap = max_bytes.saturating_add(max_bytes);
    let bytes = match blob_store.read_prefix(blob_ref, cap) {
        Ok(bytes) => bytes,
        Err(_) => return (String::new(), false),
    };
    let end = if bytes.len() <= max_bytes {
        bytes.len()
    } else {
        bytes[max_bytes..]
            .iter()
            .position(|&b| b == b'\n')
            .map(|index| max_bytes + index + 1)
            .unwrap_or(bytes.len())
    };
    let more = bytes.len() > end;
    (String::from_utf8_lossy(&bytes[..end]).into_owned(), more)
}
