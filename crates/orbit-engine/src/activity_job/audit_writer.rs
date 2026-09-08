use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::ThreadId;

use chrono::Utc;
use orbit_agent::loop_engine::audit::{AuditSink, LoopAuditEvent};
use orbit_common::OrbitError;
use orbit_types::workflow::activity_job::{
    AUDIT_ENVELOPE_SCHEMA_VERSION, V2AuditEnvelope, V2AuditEvent, V2AuditEventKind,
};
use thiserror::Error;

use orbit_store::contracts::V2AuditStoreBackend;

use super::sqlite_sink::V2SqliteSink;

/// Persistence sink for §7 audit envelopes. Abstracted behind a trait
/// ([ORB-00414]) so the writer can be exercised with an injected failing sink
/// and so envelope persistence failures can be surfaced rather than swallowed.
pub trait EnvelopeSink: Send + Sync {
    /// Persist one envelope event. An `Err` is recorded by the writer as a
    /// non-fatal audit failure rather than crashing the run.
    fn write_envelope(&self, event: &V2AuditEvent) -> Result<(), OrbitError>;
}

impl EnvelopeSink for V2SqliteSink {
    fn write_envelope(&self, event: &V2AuditEvent) -> Result<(), OrbitError> {
        V2SqliteSink::write_envelope(self, event)
    }
}

/// Writes §7 v2 audit envelope events. Nests the existing loop-engine events
/// underneath an Activity event via `parent_event_id` so the whole tree
/// (Run → Step → Activity → http.*/tool.call.*) is traversable by ID.
///
/// This writer owns the run_id / agent_identity context and emits events both
/// as structured JSON (for orbit-audit consumers) and as an inner loop sink
/// passthrough (so loop-level http.* and tool.call.* events continue to flow
/// through the existing JSONL path).
pub struct V2AuditWriter {
    run_id: String,
    agent_identity: String,
    workspace_path: Option<String>,
    inner: Arc<dyn AuditSink>,
    envelope_sink: Option<Arc<dyn EnvelopeSink>>,
    #[cfg(any(test, feature = "test-support"))]
    events: Mutex<Vec<V2AuditEvent>>,
    emitted_event_count: AtomicU64,
    event_counter: Mutex<u64>,
    parent_stacks: Mutex<HashMap<ThreadId, Vec<String>>>,
    /// [ORB-00414] Count of audit-write failures observed this run. Non-fatal
    /// to the run, but recorded so consumers know the trail is incomplete.
    audit_failures: AtomicU64,
    /// [ORB-10367] Count of telemetry-persistence failures observed this run
    /// (invocation traces). Non-fatal to the run — its success is decided by
    /// its work — but recorded so the telemetry gap is visible.
    telemetry_failures: AtomicU64,
}

/// Restores the calling thread's previous parent stack on drop.
pub(crate) struct ParentStackGuard<'a> {
    writer: &'a V2AuditWriter,
    thread_id: ThreadId,
    previous: Option<Vec<String>>,
}

#[derive(Debug, Error)]
pub enum WriteError {
    #[error("audit writer mutex poisoned")]
    Poisoned,
}

impl V2AuditWriter {
    pub fn new(
        run_id: impl Into<String>,
        agent_identity: impl Into<String>,
        inner: Arc<dyn AuditSink>,
    ) -> Self {
        Self {
            run_id: run_id.into(),
            agent_identity: agent_identity.into(),
            workspace_path: None,
            inner,
            envelope_sink: None,
            #[cfg(any(test, feature = "test-support"))]
            events: Mutex::new(Vec::new()),
            emitted_event_count: AtomicU64::new(0),
            event_counter: Mutex::new(0),
            parent_stacks: Mutex::new(HashMap::new()),
            audit_failures: AtomicU64::new(0),
            telemetry_failures: AtomicU64::new(0),
        }
    }

    /// Attach a SQLite sink for §7 envelope events. When set, every emitted
    /// envelope event is persisted alongside the in-memory snapshot.
    pub fn with_envelope_sink(mut self, sink: Arc<dyn EnvelopeSink>) -> Self {
        self.envelope_sink = Some(sink);
        self
    }

    /// Attach the originating workspace path for §7 `workspace_path`
    /// provenance. Call before the writer is shared (`Arc::new`). Absent
    /// when the caller has no meaningful workspace (stub hosts, smokes).
    pub fn with_workspace_path(mut self, path: impl Into<String>) -> Self {
        self.workspace_path = Some(path.into());
        self
    }

    /// High-level constructor for CLI / library callers that don't want to
    /// name the loop-level sink types directly (orbit-core's primary use
    /// case). Creates one SQLite-backed sink for both loop events and v2
    /// envelopes, while preserving content-addressed audit blobs under
    /// `audit_root/blobs/`.
    ///
    /// Callers that need a custom sink configuration use `new` +
    /// `with_envelope_sink` directly.
    pub fn with_disk_sinks(
        audit_root: &Path,
        store: Arc<dyn V2AuditStoreBackend>,
        workspace_id: impl Into<String>,
        run_id: impl Into<String>,
        agent_identity: impl Into<String>,
        workspace_path: Option<&Path>,
    ) -> std::io::Result<Arc<Self>> {
        let run_id = run_id.into();
        let agent_identity = agent_identity.into();
        let workspace_path_string = workspace_path.map(|path| path.display().to_string());
        let sqlite_sink = Arc::new(V2SqliteSink::for_audit_root(
            store,
            workspace_id,
            run_id.clone(),
            agent_identity.clone(),
            workspace_path_string.clone(),
            audit_root,
        ));
        let inner: Arc<dyn AuditSink> = sqlite_sink.clone();
        let mut writer = Self::new(run_id, agent_identity, inner).with_envelope_sink(sqlite_sink);
        if let Some(path) = workspace_path {
            writer = writer.with_workspace_path(path.display().to_string());
        }
        Ok(Arc::new(writer))
    }

    /// Legacy JSONL path hook. SQLite-backed audit persistence has no envelope
    /// log path, so callers should treat `None` as the expected production value.
    pub fn envelope_log_path(&self) -> Option<std::path::PathBuf> {
        None
    }

    /// Run identifier carried in every emitted envelope. Exposed so dual-write
    /// helpers (e.g. `job_executor::audit::emit_job_event`) can stamp `run_id` onto
    /// paired tracing events without re-threading the value from call sites.
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Emit a v2 envelope event of the given kind. Returns the event_id so
    /// callers can use it as a parent for nested events.
    pub fn emit(&self, kind: V2AuditEventKind) -> Result<String, WriteError> {
        let event_id = self.next_event_id()?;
        let parent_event_id = self.current_parent_event_id()?;
        let event_type = event_type_of(&kind).to_string();
        let envelope = V2AuditEnvelope {
            schema_version: AUDIT_ENVELOPE_SCHEMA_VERSION,
            event_type,
            event_id: event_id.clone(),
            ts: Utc::now(),
            run_id: self.run_id.clone(),
            agent_identity: self.agent_identity.clone(),
            parent_event_id,
            workspace_path: self.workspace_path.clone(),
        };
        let event = V2AuditEvent { envelope, kind };
        if let Some(sink) = &self.envelope_sink
            && let Err(error) = sink.write_envelope(&event)
        {
            // [ORB-00414] SQLite persistence failures should not crash the run,
            // but must be observable: record the failure (counter + tracing
            // error) instead of swallowing it. Emitting the event to the
            // Event emission is still counted below, so run results reflect
            // the complete set of attempted envelope writes.
            self.note_audit_failure(event.envelope.event_type.as_str(), &error);
        }
        #[cfg(any(test, feature = "test-support"))]
        self.events
            .lock()
            .map_err(|_| WriteError::Poisoned)?
            .push(event);

        self.emitted_event_count.fetch_add(1, Ordering::Relaxed);
        Ok(event_id)
    }

    /// [ORB-00414] Record a non-fatal audit-write failure: bump the per-run
    /// failure counter and emit a `tracing::error!` naming the run and event
    /// kind so a degraded audit trail is observable to log/JSONL consumers.
    pub(crate) fn note_audit_failure(&self, event_kind: &str, error: &dyn std::fmt::Display) {
        self.audit_failures.fetch_add(1, Ordering::Relaxed);
        tracing::error!(
            target: "orbit.engine.audit",
            run_id = %self.run_id,
            event_kind,
            error = %error,
            "audit write failed; run continuing with degraded audit trail",
        );
    }

    /// Number of audit-write failures observed this run.
    pub fn audit_failure_count(&self) -> u64 {
        self.audit_failures.load(Ordering::Relaxed)
    }

    /// True when at least one audit write failed — the trail is incomplete.
    pub fn degraded_audit(&self) -> bool {
        self.audit_failure_count() > 0
    }

    /// [ORB-10367] Record a non-fatal telemetry-persistence failure: bump the
    /// per-run counter, emit a `tracing::error!`, and put a
    /// `telemetry.persist_failed` event on the run record so the gap is
    /// visible to run-history consumers. The run's own success is never
    /// decided by this — completed agent work must not be discarded because a
    /// telemetry row could not be written.
    pub fn note_telemetry_failure(
        &self,
        component: &str,
        step_id: Option<&str>,
        error: &dyn std::fmt::Display,
    ) {
        self.telemetry_failures.fetch_add(1, Ordering::Relaxed);
        let error = error.to_string();
        tracing::error!(
            target: "orbit.engine.telemetry",
            run_id = %self.run_id,
            component,
            step_id = step_id.unwrap_or_default(),
            error = %error,
            "telemetry persist failed; run continuing with degraded telemetry",
        );
        self.emit_lossy(V2AuditEventKind::TelemetryPersistFailed {
            component: component.to_string(),
            step_id: step_id.map(ToOwned::to_owned),
            error,
        });
    }

    /// Number of telemetry-persistence failures observed this run.
    pub fn telemetry_failure_count(&self) -> u64 {
        self.telemetry_failures.load(Ordering::Relaxed)
    }

    /// True when at least one telemetry write failed — invocation traces for
    /// this run are incomplete.
    pub fn degraded_telemetry(&self) -> bool {
        self.telemetry_failure_count() > 0
    }

    /// [ORB-00414] Emit an envelope event, recording (not discarding) a write
    /// failure. Non-fatal: returns the event_id on success, `None` on failure.
    /// Used at emission sites whose event id is not load-bearing for parent
    /// nesting.
    pub(crate) fn emit_lossy(&self, kind: V2AuditEventKind) -> Option<String> {
        let event_kind = kind.event_type();
        match self.emit(kind) {
            Ok(event_id) => Some(event_id),
            Err(error) => {
                self.note_audit_failure(event_kind, &error);
                None
            }
        }
    }

    /// [ORB-00414] Push a parent context, recording a failure instead of
    /// discarding it. Non-fatal.
    pub(crate) fn push_parent_lossy(&self, event_id: String) {
        if let Err(error) = self.push_parent(event_id) {
            self.note_audit_failure("push_parent", &error);
        }
    }

    /// [ORB-00414] Pop the most recent parent context, recording a failure
    /// instead of discarding it. Non-fatal.
    pub(crate) fn pop_parent_lossy(&self) {
        if let Err(error) = self.pop_parent() {
            self.note_audit_failure("pop_parent", &error);
        }
    }

    /// Push a parent context so subsequent events nest beneath it.
    pub fn push_parent(&self, event_id: String) -> Result<(), WriteError> {
        let thread_id = std::thread::current().id();
        self.parent_stacks
            .lock()
            .map_err(|_| WriteError::Poisoned)?
            .entry(thread_id)
            .or_default()
            .push(event_id);
        Ok(())
    }

    /// Pop the most recent parent context.
    pub fn pop_parent(&self) -> Result<Option<String>, WriteError> {
        let thread_id = std::thread::current().id();
        let mut stacks = self
            .parent_stacks
            .lock()
            .map_err(|_| WriteError::Poisoned)?;
        let popped = {
            let stack = stacks.entry(thread_id).or_default();
            stack.pop()
        };
        if stacks.get(&thread_id).is_some_and(Vec::is_empty) {
            stacks.remove(&thread_id);
        }
        Ok(popped)
    }

    /// Snapshot the current thread's parent stack so callers can propagate
    /// parentage into spawned worker threads.
    pub(crate) fn parent_stack_snapshot(&self) -> Result<Vec<String>, WriteError> {
        let thread_id = std::thread::current().id();
        Ok(self
            .parent_stacks
            .lock()
            .map_err(|_| WriteError::Poisoned)?
            .get(&thread_id)
            .cloned()
            .unwrap_or_default())
    }

    /// Install a parent stack for the current thread and restore the previous
    /// value when the returned guard is dropped.
    pub(crate) fn install_parent_stack(
        &self,
        stack: Vec<String>,
    ) -> Result<ParentStackGuard<'_>, WriteError> {
        let thread_id = std::thread::current().id();
        let previous = self
            .parent_stacks
            .lock()
            .map_err(|_| WriteError::Poisoned)?
            .insert(thread_id, stack);
        Ok(ParentStackGuard {
            writer: self,
            thread_id,
            previous,
        })
    }

    /// Number of envelope events emitted during this run.
    pub fn emitted_event_count(&self) -> u64 {
        self.emitted_event_count.load(Ordering::Relaxed)
    }

    /// Snapshot of emitted events, retained only for tests that inspect
    /// envelope contents.
    #[cfg(any(test, feature = "test-support"))]
    pub fn events_snapshot(&self) -> Result<Vec<V2AuditEvent>, WriteError> {
        Ok(self
            .events
            .lock()
            .map_err(|_| WriteError::Poisoned)?
            .clone())
    }

    /// Access to the inner loop-level sink for the loop engine to emit
    /// http.*/tool.call.* events through. Returns a cloned `Arc` so callers
    /// (e.g. `EnforcedAuditSink`) can share ownership without lifetime
    /// gymnastics.
    pub fn inner_sink(&self) -> Arc<dyn AuditSink> {
        Arc::clone(&self.inner)
    }

    /// Proxy: write a blob via the inner sink (sha256-based, per §7.4 / §12 Q11).
    pub fn write_blob(&self, content: &[u8]) -> String {
        self.inner.write_blob(content)
    }

    /// Proxy: emit a loop-level event through the inner sink.
    pub fn emit_loop_event(&self, event: &LoopAuditEvent) {
        self.inner.emit(event);
    }

    fn next_event_id(&self) -> Result<String, WriteError> {
        let mut counter = self
            .event_counter
            .lock()
            .map_err(|_| WriteError::Poisoned)?;
        *counter += 1;
        Ok(format!("v2evt-{}-{:08x}", self.run_id, *counter))
    }

    fn current_parent_event_id(&self) -> Result<Option<String>, WriteError> {
        let thread_id = std::thread::current().id();
        Ok(self
            .parent_stacks
            .lock()
            .map_err(|_| WriteError::Poisoned)?
            .get(&thread_id)
            .and_then(|stack| stack.last().cloned()))
    }
}

impl Drop for ParentStackGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut stacks) = self.writer.parent_stacks.lock() {
            match self.previous.take() {
                Some(previous) if !previous.is_empty() => {
                    stacks.insert(self.thread_id, previous);
                }
                _ => {
                    stacks.remove(&self.thread_id);
                }
            }
        }
    }
}

fn event_type_of(kind: &V2AuditEventKind) -> &'static str {
    kind.event_type()
}
