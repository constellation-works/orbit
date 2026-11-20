//! Pure projections of a run's already-read v2 audit trail: steps, provider
//! processes, recovery attempts, and invocation blob previews.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use orbit_common::process::identity::{ProcessLiveness, probe_process_stopped};
use orbit_common::security::redaction::redact_all;
use orbit_common::storage::blob_store::BlobStore;

use super::body::{RunTrailBody, RunTrailEnvelope, RunTrailRow};
use super::outcome::AuditStepOutcome;
use super::trail::{
    MAX_RECOVERY_ATTEMPTS, RunAuditEvent, RunAuditStep, RunCliInvocationRecord, RunProviderProcess,
    RunRecoveryAttempt, RunRecoveryAttempts, RunRecoveryDecision, RunStoppedDescendant,
};

const MAX_RECOVERY_DIAGNOSTIC_CHARS: usize = 1024;

pub(super) fn latest_timestamp_from_envelope_rows(
    rows: impl IntoIterator<Item = orbit_store::V2AuditEventRow>,
) -> Option<DateTime<Utc>> {
    rows.into_iter()
        .filter_map(|row| RunTrailRow::parse(&row.payload_json))
        // Match the full audit projection's envelope validity boundary without
        // reconstructing parent links or activity steps just to read `ts`.
        .filter(|row| row.envelope.event_id.is_some())
        .filter_map(|row| row.envelope.ts)
        .max()
}

/// Reconstruct a run's activity steps from an already-read audit trail, in
/// first-started order.
pub(super) fn audit_steps_from_events(events: &[RunAuditEvent]) -> Vec<RunAuditStep> {
    let mut steps = Vec::<RunAuditStep>::new();
    let mut index_by_id = HashMap::<String, usize>::new();

    for event in events {
        match &event.body {
            RunTrailBody::StepStarted {
                step_id: Some(step_id),
            } => {
                if index_by_id.contains_key(step_id) {
                    continue;
                }
                let index = steps.len();
                index_by_id.insert(step_id.clone(), index);
                steps.push(RunAuditStep {
                    lock_wait_ms: None,
                    step_index: index as u32,
                    step_id: step_id.clone(),
                    started_at: event.timestamp,
                    finished_at: None,
                    outcome: None,
                    error_message: None,
                    error_diagnostic: None,
                });
            }
            RunTrailBody::StepFinished {
                step_id: Some(step_id),
                outcome,
                error_message,
                error_diagnostic,
                lock_wait_ms,
            } => {
                let Some(step) = started_step(&mut steps, &index_by_id, step_id) else {
                    continue;
                };
                step.finished_at = event.timestamp;
                step.error_diagnostic = error_diagnostic.clone();
                step.lock_wait_ms = *lock_wait_ms;
                step.outcome = Some(
                    outcome
                        .as_deref()
                        .map(AuditStepOutcome::from_recorded)
                        .unwrap_or(AuditStepOutcome::Finished),
                );
                step.error_message = error_message.clone();
            }
            RunTrailBody::StepSkipped {
                step_id: Some(step_id),
                reason,
            } => {
                let Some(step) = started_step(&mut steps, &index_by_id, step_id) else {
                    continue;
                };
                step.finished_at = event.timestamp;
                step.outcome = Some(AuditStepOutcome::Skipped);
                step.error_message = reason.clone();
            }
            RunTrailBody::StepDenied {
                step_id: Some(step_id),
                reason,
            } => {
                let Some(step) = started_step(&mut steps, &index_by_id, step_id) else {
                    continue;
                };
                step.finished_at = event.timestamp;
                step.outcome = Some(AuditStepOutcome::Denied);
                step.error_message = reason.clone();
            }
            _ => {}
        }
    }

    steps
}

/// The step a finish, skip or denial closes, when that step has started.
fn started_step<'a>(
    steps: &'a mut [RunAuditStep],
    index_by_id: &HashMap<String, usize>,
    step_id: &str,
) -> Option<&'a mut RunAuditStep> {
    let index = index_by_id.get(step_id).copied()?;
    steps.get_mut(index)
}

/// Preserve each finished attempt instead of overwriting earlier completions
/// when a step is resumed. Step indices still follow first-started order.
pub(super) fn audit_step_attempts_from_events(events: &[RunAuditEvent]) -> Vec<RunAuditStep> {
    let mut starts = HashMap::new();
    let mut attempts = Vec::new();
    for event in events {
        match &event.body {
            RunTrailBody::StepStarted {
                step_id: Some(step_id),
            } => {
                let index = starts.len() as u32;
                let start = starts.entry(step_id.as_str()).or_insert((index, None));
                start.1 = event.timestamp;
            }
            RunTrailBody::StepFinished {
                step_id: Some(step_id),
                outcome,
                error_message,
                error_diagnostic,
                lock_wait_ms,
            } => {
                let index = starts.len() as u32;
                let start = starts.entry(step_id.as_str()).or_insert((index, None));
                attempts.push(RunAuditStep {
                    error_diagnostic: error_diagnostic.clone(),
                    lock_wait_ms: *lock_wait_ms,
                    step_index: start.0,
                    step_id: step_id.clone(),
                    started_at: start.1,
                    finished_at: event.timestamp,
                    outcome: outcome.as_deref().map(AuditStepOutcome::from_recorded),
                    error_message: error_message.clone(),
                });
                start.1 = event.timestamp;
            }
            _ => {}
        }
    }
    attempts
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
        match &event.body {
            RunTrailBody::CliInvocationProcess {
                run_id: event_run_id,
                provider,
                pid: Some(pid),
                pid_start_time,
            } => {
                let step_index = event
                    .step_id
                    .as_ref()
                    .and_then(|step_id| step_index_by_id.get(step_id).copied());
                if let Some(parent_event_id) = &event.parent_event_id {
                    invocation_parent_by_process_event
                        .insert(event.event_id.clone(), parent_event_id.clone());
                }
                records.push(RunProviderProcess {
                    run_id: event_run_id.as_deref().unwrap_or(run_id).to_string(),
                    event_id: event.event_id.clone(),
                    ts: event.timestamp,
                    step_index,
                    step_id: event.step_id.clone(),
                    provider: provider.clone(),
                    pid: *pid,
                    pid_start_time: pid_start_time.clone(),
                    finished: false,
                    exit_code: None,
                    timed_out: false,
                    duration_ms: None,
                    // Overwritten below; only unfinished records are probed.
                    liveness: ProcessLiveness::Exited,
                    last_activity_at: None,
                    latest_message: None,
                    latest_message_truncated: false,
                    stdout_blob_ref: None,
                    build_budget_waits: None,
                    stopped_descendants: Vec::new(),
                });
            }
            RunTrailBody::CliInvocationStoppedDescendant {
                pid: Some(pid),
                pid_start_time,
                command,
                stopped_ms,
                ended,
                error,
            } => {
                let Some(record) = matching_provider_process_for_completion(
                    &mut records,
                    &invocation_parent_by_process_event,
                    &event,
                ) else {
                    continue;
                };
                record.stopped_descendants.push(RunStoppedDescendant {
                    ts: event.timestamp,
                    pid: *pid,
                    pid_start_time: pid_start_time.clone(),
                    command: command.clone(),
                    stopped_ms: *stopped_ms,
                    ended: *ended,
                    error: error.clone(),
                    // Probed below, for open children only.
                    still_stopped: false,
                });
            }
            RunTrailBody::CliInvocationActivity {
                latest_message,
                latest_message_truncated,
            } => {
                // Correlated exactly as a completion is: the open child the
                // same invocation spawned.
                let Some(record) = matching_provider_process_for_completion(
                    &mut records,
                    &invocation_parent_by_process_event,
                    &event,
                ) else {
                    continue;
                };
                record.last_activity_at = event.timestamp.or(record.last_activity_at);
                // A sample whose tail held no message keeps the previous one.
                if let Some(message) = latest_message {
                    record.latest_message = Some(message.clone());
                    record.latest_message_truncated = *latest_message_truncated;
                }
            }
            RunTrailBody::CliInvocationBuildBudget(waits) => {
                let Some(record) = matching_provider_process_for_completion(
                    &mut records,
                    &invocation_parent_by_process_event,
                    &event,
                ) else {
                    continue;
                };
                record.build_budget_waits = waits.clone();
            }
            RunTrailBody::CliInvocationFinished {
                exit_code,
                timed_out,
                duration_ms,
                stdout_blob_ref,
                ..
            } => {
                let Some(record) = matching_provider_process_for_completion(
                    &mut records,
                    &invocation_parent_by_process_event,
                    &event,
                ) else {
                    continue;
                };
                record.finished = true;
                record.exit_code = *exit_code;
                record.timed_out = *timed_out;
                record.duration_ms = *duration_ms;
                record.stdout_blob_ref = stdout_blob_ref.clone();
            }
            _ => {}
        }
    }

    for record in &mut records {
        if !record.finished {
            record.liveness = probe(record.pid, record.pid_start_time.as_deref());
            for descendant in &mut record.stopped_descendants {
                descendant.still_stopped =
                    probe_process_stopped(descendant.pid, descendant.pid_start_time.as_deref());
            }
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

/// One envelope event of a run's trail. `events_by_id` holds the trail rows
/// read so far, through which an event without its own `step_id` finds the
/// step that encloses it.
pub(super) fn run_audit_event(
    event_id: String,
    row: RunTrailRow,
    events_by_id: &HashMap<String, RunTrailRow>,
) -> RunAuditEvent {
    let step_id = enclosing_step_id(&row.envelope, events_by_id);
    let RunTrailRow {
        raw,
        envelope,
        body,
    } = row;
    RunAuditEvent {
        parent_event_id: envelope.parent_event_id,
        event_type: envelope.event_type,
        body,
        timestamp: envelope.ts,
        step_id,
        raw,
        event_id,
    }
}

/// The operator record of one `cli_invocation_finished` event, reading its
/// stdout/stderr blobs whole or, given `blob_preview_max_bytes`, as a preview.
/// `None` for any other kind of event.
pub(super) fn cli_invocation_record(
    run_id: &str,
    event: RunAuditEvent,
    blob_store: &BlobStore,
    blob_preview_max_bytes: Option<usize>,
    step_index_by_id: &HashMap<String, u32>,
) -> Option<RunCliInvocationRecord> {
    let RunTrailBody::CliInvocationFinished {
        run_id: event_run_id,
        provider,
        exit_code,
        timed_out,
        duration_ms,
        stdout_blob_ref,
        stderr_blob_ref,
    } = event.body
    else {
        return None;
    };
    let (stdout, stdout_blob_truncated) = read_invocation_blob(
        blob_store,
        stdout_blob_ref.as_deref(),
        blob_preview_max_bytes,
    );
    let (stderr, stderr_blob_truncated) = read_invocation_blob(
        blob_store,
        stderr_blob_ref.as_deref(),
        blob_preview_max_bytes,
    );
    let step_index = event
        .step_id
        .as_ref()
        .and_then(|step_id| step_index_by_id.get(step_id).copied());
    Some(RunCliInvocationRecord {
        run_id: event_run_id.unwrap_or_else(|| run_id.to_string()),
        step_index,
        provider,
        exit_code,
        timed_out,
        duration_ms,
        event_id: event.event_id,
        ts: event.timestamp,
        step_id: event.step_id,
        stdout_blob_ref,
        stderr_blob_ref,
        stdout,
        stderr,
        stdout_blob_truncated,
        stderr_blob_truncated,
    })
}

fn enclosing_step_id(
    envelope: &RunTrailEnvelope,
    events: &HashMap<String, RunTrailRow>,
) -> Option<String> {
    if let Some(step_id) = &envelope.step_id {
        return Some(step_id.clone());
    }

    let mut parent_id = envelope.parent_event_id.clone();
    let mut seen = HashSet::new();
    while let Some(id) = parent_id {
        if !seen.insert(id.clone()) {
            return None;
        }
        let parent = events.get(&id)?;
        if let RunTrailBody::StepStarted { step_id } = &parent.body {
            return step_id.clone();
        }
        parent_id = parent.envelope.parent_event_id.clone();
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
    let RunTrailRow {
        raw,
        envelope,
        body,
    } = RunTrailRow::parse(&row.payload_json)?;
    Some(RunAuditEvent {
        event_id: envelope.event_id?,
        parent_event_id: envelope.parent_event_id,
        event_type: envelope.event_type,
        body,
        timestamp: envelope.ts.or(Some(row.ts)),
        step_id: envelope.step_id,
        raw,
    })
}

fn recovery_attempt_from_event(run_id: &str, event: RunAuditEvent) -> Option<RunRecoveryAttempt> {
    match event.body {
        RunTrailBody::FinalRecoveryAttempted {
            step_id,
            final_recovery_activity,
            outcome,
            decision,
            detail,
        } => {
            let failed_step_id = step_id?;
            let outcome = outcome?;
            let recovery_activity = final_recovery_activity?;
            let detail = detail.as_deref().map(bounded_recovery_diagnostic);
            Some(RunRecoveryAttempt {
                run_id: run_id.to_string(),
                event_id: event.event_id,
                attempted_at: event.timestamp,
                failed_step_id,
                recovery_activity,
                retry_admitted: outcome == "resume",
                outcome,
                failure_phase: None,
                diagnostic: detail.as_ref().map(|(text, _)| text.clone()),
                diagnostic_truncated: detail.as_ref().is_some_and(|(_, truncated)| *truncated),
                decision: decision.map(|verdict| RunRecoveryDecision {
                    status: "verified".to_string(),
                    verdict: Some(verdict),
                    detail: detail.map(|(text, _)| text),
                }),
            })
        }
        RunTrailBody::StepRecoveryAttempted {
            run_id: event_run_id,
            step_id,
            recovery_activity,
            recovery_succeeded,
            failure_phase,
            error_message,
            decision,
        } => {
            let failed_step_id = step_id?;
            let recovery_activity = recovery_activity?;
            let recovery_succeeded = recovery_succeeded?;
            let (diagnostic, diagnostic_truncated) = error_message
                .as_deref()
                .map(bounded_recovery_diagnostic)
                .map_or((None, false), |(diagnostic, truncated)| {
                    (Some(diagnostic), truncated)
                });
            let decision = decision.map(|decision| {
                (
                    RunRecoveryDecision {
                        status: decision.status,
                        verdict: decision.verdict,
                        detail: decision
                            .detail
                            .as_deref()
                            .map(|detail| bounded_recovery_diagnostic(detail).0),
                    },
                    decision.retry_admitted,
                )
            });
            // A historical or slotless completion kept the single re-attempt.
            let retry_admitted =
                recovery_succeeded && decision.as_ref().is_none_or(|(_, admitted)| *admitted);

            Some(RunRecoveryAttempt {
                run_id: event_run_id.unwrap_or_else(|| run_id.to_string()),
                event_id: event.event_id,
                attempted_at: event.timestamp,
                failed_step_id,
                recovery_activity,
                outcome: if recovery_succeeded {
                    "succeeded".to_string()
                } else {
                    "failed".to_string()
                },
                failure_phase,
                diagnostic,
                diagnostic_truncated,
                decision: decision.map(|(decision, _)| decision),
                retry_admitted,
            })
        }
        _ => None,
    }
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
fn read_invocation_blob(
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
