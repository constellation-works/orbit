use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::process::identity::{ProcessLiveness, probe_process_liveness};
use orbit_common::storage::blob_store::BlobStore;
use serde_json::Value;

use crate::{OrbitRuntime, V2AuditEventFilter};

use super::run_projection::{
    audit_steps_from_events, bound_provider_processes, enclosing_step_id,
    latest_timestamp_from_envelope_rows, provider_processes_from_events, read_invocation_blob,
    recovery_attempts_from_partitioned_rows, step_index_by_id,
};

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
    /// True when
    /// [`read_blob_text_preview_best_effort`](super::run_projection::read_blob_text_preview_best_effort)
    /// cut the blob before its end. Independent of the caller's own
    /// line-budget truncation check, which cannot see past whatever window
    /// was read here.
    pub stdout_blob_truncated: bool,
    pub stderr_blob_truncated: bool,
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
/// Rows per store read while reconstructing a complete run audit trail.
///
/// This is a paging size, not an evidence limit. Safety decisions such as
/// orphan reconciliation must see provider spawns even in unusually long
/// trails, while each SQLite read should remain bounded.
#[cfg(not(test))]
const RUN_AUDIT_PAGE_SIZE: usize = 50_000;
#[cfg(test)]
const RUN_AUDIT_PAGE_SIZE: usize = 8;

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
        let mut rows = Vec::new();
        let mut offset = 0;
        loop {
            let page = self.list_v2_audit_events(V2AuditEventFilter {
                workspace_id: String::new(),
                run_id: Some(run_id.to_string()),
                source: Some("v2_envelope".to_string()),
                limit: Some(RUN_AUDIT_PAGE_SIZE),
                offset: Some(offset),
                ..Default::default()
            })?;
            let page_len = page.len();
            rows.extend(page);
            if page_len < RUN_AUDIT_PAGE_SIZE {
                break;
            }
            offset += page_len;
        }
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

    /// The newest valid timestamp carried by the newest v2 envelope rows for
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
            limit: Some(RUN_AUDIT_PAGE_SIZE),
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
        self.collect_run_cli_invocations_bounded(run_id, None, None)
    }

    /// Collect CLI invocation records for a run, optionally stopping after
    /// `limit` invocations and reading only a preview window of each blob.
    ///
    /// The unbounded wrapper [`Self::collect_run_cli_invocations`] still loads
    /// every invocation's full stdout/stderr. Callers that only render a
    /// truncated preview should pass both bounds so a long run is not charged
    /// a full multi-MB blob read per invocation.
    pub fn collect_run_cli_invocations_bounded(
        &self,
        run_id: &str,
        limit: Option<usize>,
        blob_preview_max_bytes: Option<usize>,
    ) -> Result<Vec<RunCliInvocationRecord>, OrbitError> {
        if limit == Some(0) {
            return Ok(Vec::new());
        }
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
            let (stdout, stdout_blob_truncated) = read_invocation_blob(
                &blob_store,
                stdout_blob_ref.as_deref(),
                blob_preview_max_bytes,
            );
            let (stderr, stderr_blob_truncated) = read_invocation_blob(
                &blob_store,
                stderr_blob_ref.as_deref(),
                blob_preview_max_bytes,
            );
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
                stdout_blob_truncated,
                stderr_blob_truncated,
                exit_code: event.raw.get("exit_code").and_then(Value::as_i64),
                timed_out: event
                    .raw
                    .get("timed_out")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                duration_ms: event.raw.get("duration_ms").and_then(Value::as_u64),
            });
            if limit.is_some_and(|limit| records.len() >= limit) {
                break;
            }
        }

        Ok(records)
    }

    fn v2_audit_blob_root(&self) -> PathBuf {
        self.data_root().join("state").join("audit").join("blobs")
    }
}
