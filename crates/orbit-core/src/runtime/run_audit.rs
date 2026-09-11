use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::process::identity::{ProcessLiveness, probe_process_liveness};
use orbit_common::security::redaction::redact_all;
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

/// A bounded, operator-facing recovery attempt reconstructed from the v2 audit
/// trail. It intentionally excludes activity input and provider transcript
/// blobs: those can contain unrelated or unbounded material and are not needed
/// to distinguish a recovery failure from the original failed workflow step.
#[derive(Clone, Debug, PartialEq)]
pub struct RunRecoveryAttempt {
    pub run_id: String,
    pub event_id: String,
    pub attempted_at: Option<DateTime<Utc>>,
    pub failed_step_id: String,
    pub recovery_activity: String,
    pub outcome: String,
    pub failure_phase: Option<String>,
    pub diagnostic: Option<String>,
    pub diagnostic_truncated: bool,
}

/// The recovery portion of a run's persisted audit trail.
///
/// `unavailable` means the run has no v2 audit evidence (common for legacy
/// runs), while `not_attempted` means audit evidence exists but records no
/// recovery attempt. Neither state is a successful recovery.
#[derive(Clone, Debug, PartialEq)]
pub struct RunRecoveryAttempts {
    pub state: &'static str,
    pub attempts: Vec<RunRecoveryAttempt>,
    pub limit: usize,
    pub truncated: bool,
}

impl RunRecoveryAttempts {
    pub fn unavailable() -> Self {
        Self {
            state: "unavailable",
            attempts: Vec::new(),
            limit: MAX_RECOVERY_ATTEMPTS,
            truncated: false,
        }
    }

    pub fn not_attempted() -> Self {
        Self {
            state: "not_attempted",
            attempts: Vec::new(),
            limit: MAX_RECOVERY_ATTEMPTS,
            truncated: false,
        }
    }
}

pub(crate) const MAX_RECOVERY_ATTEMPTS: usize = 8;
/// One extra recovery row per run so `truncated` is visible without a
/// page-wide LIMIT that would starve later runs [ORB-11625].
pub(crate) const RECOVERY_FETCH_PER_RUN: usize = MAX_RECOVERY_ATTEMPTS + 1;
const MAX_RECOVERY_DIAGNOSTIC_CHARS: usize = 1024;

/// Recovery evidence for a list page, plus the query counts the list
/// projection uses to prove it did not scan one full envelope per run.
#[derive(Clone, Debug, PartialEq)]
pub struct RunRecoveryAttemptsPage {
    pub by_run_id: HashMap<String, RunRecoveryAttempts>,
    pub event_queries: usize,
    pub presence_queries: usize,
    pub per_run_fetch_limit: usize,
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

impl RunProviderProcess {
    /// The operator-facing projection of one provider child.
    ///
    /// Shared by the CLI and the registered/MCP run-show surfaces so both
    /// readers name the same process with the same keys; a child that is alive
    /// in one and absent from the other is the failure this projection exists
    /// to prevent. `run_id` is omitted: every caller already knows the run it
    /// asked about.
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "event_id": self.event_id,
            "ts": self.ts.map(|ts| ts.to_rfc3339()),
            "step_id": self.step_id,
            "step_index": self.step_index,
            "provider": self.provider,
            "pid": self.pid,
            "pid_start_time": self.pid_start_time,
            "finished": self.finished,
            "liveness": self.liveness.as_str(),
            "exit_code": self.exit_code,
            "timed_out": self.timed_out,
            "duration_ms": self.duration_ms,
        })
    }
}

/// [ORB-11752] What a run is doing *right now*: the activity step that is open
/// and the provider children it spawned, each with a liveness verdict.
///
/// This is the evidence that separates a healthy implementation agent from an
/// abandoned wrapper. `state` is `observed` when the run has a v2 audit trail
/// and `unavailable` when it has none — a legacy run, or one whose trail was
/// never written. An `observed` progress with no active step and no open child
/// is a run between steps, which is a different fact from having no evidence.
#[derive(Clone, Debug, PartialEq)]
pub struct RunExecutionProgress {
    pub state: &'static str,
    pub active_step: Option<RunAuditStep>,
    pub provider_processes: Vec<RunProviderProcess>,
    pub limit: usize,
    pub truncated: bool,
}

impl RunExecutionProgress {
    /// The empty projection: no v2 audit trail, or one this reader could not
    /// open. Never confused with "nothing is running".
    pub fn unavailable() -> Self {
        Self {
            state: "unavailable",
            active_step: None,
            provider_processes: Vec::new(),
            limit: MAX_PROVIDER_PROCESSES,
            truncated: false,
        }
    }
}

/// Provider children carried on the progress projection. A run spawns a
/// handful per step; the budget only bites on a long retry history.
const MAX_PROVIDER_PROCESSES: usize = 8;

/// What one scan of a run's v2 audit trail says about its execution: every
/// step it started, and every provider child those steps spawned.
///
/// Unlike [`RunExecutionProgress`], the step list is complete rather than
/// narrowed to the open step, and the provider list is unbounded — this is the
/// evidence `orbit run show` renders, not a live-progress summary.
#[derive(Clone, Debug, PartialEq)]
pub struct RunAuditView {
    pub steps: Vec<RunAuditStep>,
    pub provider_processes: Vec<RunProviderProcess>,
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
        Ok(self
            .collect_run_audit_view_with(run_id, probe)?
            .provider_processes)
    }

    /// Both audit-derived halves of a run inspection — its reconstructed steps
    /// and the provider children its activities spawned — from one scan.
    ///
    /// `orbit run show` needs the steps because a v2 pipeline run keeps them
    /// only here: its job-run record's `steps` are empty while the same trail
    /// answers `orbit run events` in full [ORB-12113]. Paying for one read
    /// rather than two is the same reason [`Self::collect_run_execution_progress`]
    /// exists.
    pub fn collect_run_audit_view(&self, run_id: &str) -> Result<RunAuditView, OrbitError> {
        self.collect_run_audit_view_with(run_id, probe_process_liveness)
    }

    /// Inner, testable form of [`Self::collect_run_audit_view`] with the
    /// liveness probe injected, so pairing and projection can be asserted
    /// without depending on real live PIDs.
    pub(crate) fn collect_run_audit_view_with<P>(
        &self,
        run_id: &str,
        probe: P,
    ) -> Result<RunAuditView, OrbitError>
    where
        P: Fn(u32, Option<&str>) -> ProcessLiveness,
    {
        let events = self.collect_run_audit_events(run_id)?;
        let steps = audit_steps_from_events(&events);
        let provider_processes =
            provider_processes_from_events(run_id, events, &step_index_by_id(&steps), probe);
        Ok(RunAuditView {
            steps,
            provider_processes,
        })
    }

    /// [ORB-11752] The run's live progress: open activity step plus a bounded,
    /// liveness-probed view of the provider children it spawned.
    ///
    /// One audit scan answers both halves, so an observer asking "is my
    /// implementation agent still there" pays for a single read rather than
    /// three overlapping ones.
    pub fn collect_run_execution_progress(
        &self,
        run_id: &str,
    ) -> Result<RunExecutionProgress, OrbitError> {
        self.collect_run_execution_progress_with(run_id, probe_process_liveness)
    }

    /// Inner, testable form of [`Self::collect_run_execution_progress`] with the
    /// liveness probe injected.
    pub(crate) fn collect_run_execution_progress_with<P>(
        &self,
        run_id: &str,
        probe: P,
    ) -> Result<RunExecutionProgress, OrbitError>
    where
        P: Fn(u32, Option<&str>) -> ProcessLiveness,
    {
        let events = self.collect_run_audit_events(run_id)?;
        if events.is_empty() {
            return Ok(RunExecutionProgress::unavailable());
        }

        let steps = audit_steps_from_events(&events);
        // Parallel activities can leave several steps open at once; the newest
        // one is what "what is it doing now" means to an operator.
        let active_step = steps
            .iter()
            .rev()
            .find(|step| step.finished_at.is_none())
            .cloned();
        let records =
            provider_processes_from_events(run_id, events, &step_index_by_id(&steps), probe);
        let (provider_processes, truncated) =
            bound_provider_processes(records, MAX_PROVIDER_PROCESSES);

        Ok(RunExecutionProgress {
            state: "observed",
            active_step,
            provider_processes,
            limit: MAX_PROVIDER_PROCESSES,
            truncated,
        })
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

    /// The newest valid timestamp carried by the bounded v2 envelope rows for
    /// a run. This deliberately reads the envelope payload rather than the
    /// audit row ordering: imports and delayed writers can persist rows out of
    /// timestamp order.
    pub(crate) fn latest_run_audit_timestamp(
        &self,
        run_id: &str,
    ) -> Result<Option<DateTime<Utc>>, OrbitError> {
        let rows = self.list_v2_audit_events(V2AuditEventFilter {
            workspace_id: String::new(),
            run_id: Some(run_id.to_string()),
            source: Some("v2_envelope".to_string()),
            limit: Some(50_000),
            ..Default::default()
        })?;

        Ok(latest_timestamp_from_envelope_rows(rows))
    }

    pub fn collect_run_audit_steps(&self, run_id: &str) -> Result<Vec<RunAuditStep>, OrbitError> {
        Ok(audit_steps_from_events(
            &self.collect_run_audit_events(run_id)?,
        ))
    }

    /// Recover the most recent bounded recovery-attempt evidence for a run.
    ///
    /// The event writer independently redacts and bounds its diagnostic, but
    /// the projection applies the same safety boundary again because durable
    /// audit rows can predate that writer behavior.
    pub fn collect_run_recovery_attempts(
        &self,
        run_id: &str,
    ) -> Result<RunRecoveryAttempts, OrbitError> {
        let page = self.collect_run_recovery_attempts_for_runs(&[run_id.to_string()])?;
        Ok(page
            .by_run_id
            .get(run_id)
            .cloned()
            .unwrap_or_else(RunRecoveryAttempts::unavailable))
    }

    /// Recovery evidence for a list page from one audit-store handle.
    ///
    /// [ORB-11625] The previous list path scanned each run's full v2 envelope
    /// (`limit: 50_000`) and opened the audit store once per row. This loads
    /// only `step_recovery_attempted` rows, capped per run at
    /// [`RECOVERY_FETCH_PER_RUN`], and a presence set for `unavailable` vs
    /// `not_attempted`. A page-wide LIMIT is not used: it would starve later
    /// runs with shorter histories. Store-handle reuse is ORB-11632; this
    /// method must not add a per-run `Store::open`.
    pub fn collect_run_recovery_attempts_for_runs(
        &self,
        run_ids: &[String],
    ) -> Result<RunRecoveryAttemptsPage, OrbitError> {
        if run_ids.is_empty() {
            return Ok(RunRecoveryAttemptsPage {
                by_run_id: HashMap::new(),
                event_queries: 0,
                presence_queries: 0,
                per_run_fetch_limit: RECOVERY_FETCH_PER_RUN,
            });
        }

        let workspace_id = self.workspace_id()?;
        let store = self.v2_audit_store()?;
        let rows = store.list_v2_audit_events_for_runs_partitioned(
            &workspace_id,
            run_ids,
            Some("v2_envelope"),
            Some("step_recovery_attempted"),
            RECOVERY_FETCH_PER_RUN,
        )?;
        let present =
            store.list_v2_audit_run_ids_with_events(&workspace_id, run_ids, Some("v2_envelope"))?;

        let mut grouped: HashMap<String, Vec<orbit_store::V2AuditEventRow>> = HashMap::new();
        for row in rows {
            grouped.entry(row.run_id.clone()).or_default().push(row);
        }

        let mut by_run_id = HashMap::new();
        for run_id in run_ids {
            let attempts = match grouped.get(run_id) {
                Some(run_rows) => recovery_attempts_from_partitioned_rows(run_id, run_rows),
                None if present.contains(run_id) => RunRecoveryAttempts::not_attempted(),
                None => RunRecoveryAttempts::unavailable(),
            };
            by_run_id.insert(run_id.clone(), attempts);
        }

        Ok(RunRecoveryAttemptsPage {
            by_run_id,
            event_queries: 1,
            presence_queries: 1,
            per_run_fetch_limit: RECOVERY_FETCH_PER_RUN,
        })
    }

    pub fn collect_run_cli_invocations(
        &self,
        run_id: &str,
    ) -> Result<Vec<RunCliInvocationRecord>, OrbitError> {
        let events = self.collect_run_audit_events(run_id)?;
        let blob_store = BlobStore::new(self.v2_audit_blob_root());
        let step_index_by_id = audit_steps_from_events(&events)
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

fn latest_timestamp_from_envelope_rows(
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
fn audit_steps_from_events(events: &[RunAuditEvent]) -> Vec<RunAuditStep> {
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
fn step_index_by_id(steps: &[RunAuditStep]) -> HashMap<String, u32> {
    steps
        .iter()
        .map(|step| (step.step_id.clone(), step.step_index))
        .collect()
}

/// Reconstruct the run's provider subprocesses from an already-read audit
/// trail, pairing each spawn with the completion that closes it and probing the
/// liveness of whatever is still open.
fn provider_processes_from_events<P>(
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
fn bound_provider_processes(
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

fn recovery_attempts_from_partitioned_rows(
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
