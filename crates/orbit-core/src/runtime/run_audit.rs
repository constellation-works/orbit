use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::process::identity::{ProcessLiveness, probe_process_liveness};
use orbit_common::storage::blob_store::BlobStore;
use serde_json::Value;

use crate::{OrbitRuntime, V2AuditEventFilter};

#[derive(Clone, Debug, PartialEq)]
pub struct RunAuditEvent {
    pub raw: Value,
    pub event_id: String,
    pub parent_event_id: Option<String>,
    pub event_type: Option<String>,
    pub body_kind: Option<String>,
    pub timestamp: Option<DateTime<Utc>>,
    pub step_id: Option<String>,
}

impl RunAuditEvent {
    pub fn json_with_step_id(&self) -> Value {
        let mut raw = self.raw.clone();
        if let Some(step_id) = &self.step_id
            && raw.get("step_id").is_none()
            && let Some(object) = raw.as_object_mut()
        {
            object.insert("step_id".to_string(), Value::String(step_id.clone()));
        }
        raw
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RunAuditStep {
    pub step_index: u32,
    pub step_id: String,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub state: Option<String>,
    pub outcome: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RunCliInvocationRecord {
    pub run_id: String,
    pub event_id: String,
    pub ts: Option<DateTime<Utc>>,
    pub step_id: Option<String>,
    pub step_index: Option<u32>,
    pub provider: Option<String>,
    pub stdout_blob_ref: Option<String>,
    pub stderr_blob_ref: Option<String>,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i64>,
    pub timed_out: bool,
    pub duration_ms: Option<u64>,
}

/// [ORB-10496] One provider subprocess spawned by a CLI-backed agent step.
///
/// Reconstructed from the run's audit trail by pairing each
/// `cli.invocation.process` event with the `cli.invocation.finished` event that
/// closes it. A record with `finished == false` is a child that had not exited
/// when the trail was last written; `liveness` says whether it is still there.
#[derive(Clone, Debug, PartialEq)]
pub struct RunProviderProcess {
    pub run_id: String,
    pub event_id: String,
    pub ts: Option<DateTime<Utc>>,
    pub step_id: Option<String>,
    pub step_index: Option<u32>,
    pub provider: Option<String>,
    pub pid: u32,
    pub pid_start_time: Option<String>,
    pub finished: bool,
    pub exit_code: Option<i64>,
    pub timed_out: bool,
    pub duration_ms: Option<u64>,
    pub liveness: ProcessLiveness,
}

impl OrbitRuntime {
    /// Provider subprocesses recorded for a run, oldest first, each with a
    /// liveness verdict for the ones that have not reported an exit.
    ///
    /// This is the only observability channel for ship-pipeline
    /// (`workflow_ship`) implementation agents: they are children of the
    /// pipeline worker, not of the Worker daemon, so they never appear in the
    /// Worker run store that `agent_run_list` reads.
    pub fn collect_run_provider_processes(
        &self,
        run_id: &str,
    ) -> Result<Vec<RunProviderProcess>, OrbitError> {
        self.collect_run_provider_processes_with(run_id, probe_process_liveness)
    }

    /// Inner, testable form of [`Self::collect_run_provider_processes`] with the
    /// liveness probe injected, so pairing and projection can be asserted
    /// without depending on real live PIDs.
    pub(crate) fn collect_run_provider_processes_with<P>(
        &self,
        run_id: &str,
        probe: P,
    ) -> Result<Vec<RunProviderProcess>, OrbitError>
    where
        P: Fn(u32, Option<&str>) -> ProcessLiveness,
    {
        let events = self.collect_run_audit_events(run_id)?;
        let step_index_by_id = self
            .collect_run_audit_steps(run_id)?
            .into_iter()
            .map(|step| (step.step_id, step.step_index))
            .collect::<HashMap<_, _>>();
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

        Ok(records)
    }

    pub fn collect_run_audit_events(&self, run_id: &str) -> Result<Vec<RunAuditEvent>, OrbitError> {
        let rows = self.list_v2_audit_events(V2AuditEventFilter {
            workspace_id: String::new(),
            run_id: Some(run_id.to_string()),
            source: Some("v2_envelope".to_string()),
            limit: Some(50_000),
            ..Default::default()
        })?;
        let mut events_by_id = HashMap::new();
        let mut ordered_ids = Vec::new();
        for row in rows.into_iter().rev() {
            let value: Value = match serde_json::from_str(&row.payload_json) {
                Ok(value) => value,
                Err(_) => continue,
            };
            let Some(event_id) = value.get("event_id").and_then(Value::as_str) else {
                continue;
            };
            ordered_ids.push(event_id.to_string());
            events_by_id.insert(event_id.to_string(), value);
        }

        let mut events = Vec::new();
        for event_id in ordered_ids {
            let Some(raw) = events_by_id.get(&event_id).cloned() else {
                continue;
            };
            events.push(RunAuditEvent {
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
                    .map(|value| value.with_timezone(&Utc)),
                step_id: enclosing_step_id(&raw, &events_by_id),
                raw,
                event_id,
            });
        }

        Ok(events)
    }

    pub fn collect_run_audit_steps(&self, run_id: &str) -> Result<Vec<RunAuditStep>, OrbitError> {
        let events = self.collect_run_audit_events(run_id)?;
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

        Ok(steps)
    }

    pub fn collect_run_cli_invocations(
        &self,
        run_id: &str,
    ) -> Result<Vec<RunCliInvocationRecord>, OrbitError> {
        let events = self.collect_run_audit_events(run_id)?;
        let blob_store = BlobStore::new(self.v2_audit_blob_root());
        let step_index_by_id = self
            .collect_run_audit_steps(run_id)?
            .into_iter()
            .map(|step| (step.step_id, step.step_index))
            .collect::<HashMap<_, _>>();
        let mut records = Vec::new();

        for event in events {
            if event.body_kind.as_deref() != Some("cli_invocation_finished") {
                continue;
            }
            let stdout_blob_ref = event
                .raw
                .get("stdout_blob_ref")
                .and_then(Value::as_str)
                .map(str::to_string);
            let stderr_blob_ref = event
                .raw
                .get("stderr_blob_ref")
                .and_then(Value::as_str)
                .map(str::to_string);
            let stdout = match stdout_blob_ref.as_deref() {
                Some(blob_ref) => read_blob_text_best_effort(&blob_store, blob_ref),
                None => String::new(),
            };
            let stderr = match stderr_blob_ref.as_deref() {
                Some(blob_ref) => read_blob_text_best_effort(&blob_store, blob_ref),
                None => String::new(),
            };
            let step_index = event
                .step_id
                .as_ref()
                .and_then(|step_id| step_index_by_id.get(step_id).copied());
            records.push(RunCliInvocationRecord {
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
                stdout_blob_ref,
                stderr_blob_ref,
                stdout,
                stderr,
                exit_code: event.raw.get("exit_code").and_then(Value::as_i64),
                timed_out: event
                    .raw
                    .get("timed_out")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                duration_ms: event.raw.get("duration_ms").and_then(Value::as_u64),
            });
        }

        Ok(records)
    }

    fn v2_audit_blob_root(&self) -> PathBuf {
        self.data_root().join("state").join("audit").join("blobs")
    }
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

fn enclosing_step_id(event: &Value, events: &HashMap<String, Value>) -> Option<String> {
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

fn read_blob_text(blob_store: &BlobStore, blob_ref: &str) -> Result<String, OrbitError> {
    if blob_ref.len() < 2 || blob_ref.starts_with("error:") {
        return Err(OrbitError::Store(format!(
            "invalid audit blob reference '{blob_ref}'"
        )));
    }
    let bytes = blob_store
        .read(blob_ref)
        .map_err(|err| OrbitError::Io(format!("read audit blob '{blob_ref}': {err}")))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn read_blob_text_best_effort(blob_store: &BlobStore, blob_ref: &str) -> String {
    read_blob_text(blob_store, blob_ref).unwrap_or_default()
}
