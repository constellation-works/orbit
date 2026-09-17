use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use orbit_agent::loop_engine::audit::{AuditSink, BlobStore, LoopAuditEvent};
use orbit_common::OrbitError;
use orbit_store::contracts::{V2AuditEventFilter, V2AuditEventInsertParams, V2AuditStoreBackend};
use orbit_types::workflow::activity_job::V2AuditEvent;

pub struct V2SqliteSink {
    store: std::sync::Arc<dyn V2AuditStoreBackend>,
    workspace_id: String,
    run_id: String,
    agent_identity: String,
    workspace_path: Option<String>,
    blob_store: BlobStore,
    loop_event_counter: Mutex<u64>,
}

impl V2SqliteSink {
    pub fn new(
        store: std::sync::Arc<dyn V2AuditStoreBackend>,
        workspace_id: impl Into<String>,
        run_id: impl Into<String>,
        agent_identity: impl Into<String>,
        workspace_path: Option<String>,
        blob_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            store,
            workspace_id: workspace_id.into(),
            run_id: run_id.into(),
            agent_identity: agent_identity.into(),
            workspace_path,
            blob_store: BlobStore::new(blob_root),
            loop_event_counter: Mutex::new(0),
        }
    }

    pub fn for_audit_root(
        store: std::sync::Arc<dyn V2AuditStoreBackend>,
        workspace_id: impl Into<String>,
        run_id: impl Into<String>,
        agent_identity: impl Into<String>,
        workspace_path: Option<String>,
        audit_root: &Path,
    ) -> Self {
        Self::new(
            store,
            workspace_id,
            run_id,
            agent_identity,
            workspace_path,
            audit_root.join("blobs"),
        )
    }

    pub fn write_envelope(&self, event: &V2AuditEvent) -> Result<(), OrbitError> {
        let payload_json = serde_json::to_string(event)
            .map_err(|err| OrbitError::Store(format!("serialize v2 audit event: {err}")))?;
        self.store.insert_v2_audit_event(&V2AuditEventInsertParams {
            workspace_id: self.workspace_id.clone(),
            event_id: event.envelope.event_id.clone(),
            source: "v2_envelope".to_string(),
            schema_version: event.envelope.schema_version,
            event_type: event.envelope.event_type.clone(),
            ts: event.envelope.ts,
            run_id: event.envelope.run_id.clone(),
            agent_identity: event.envelope.agent_identity.clone(),
            parent_event_id: event.envelope.parent_event_id.clone(),
            workspace_path: event.envelope.workspace_path.clone(),
            payload_json,
        })
    }

    pub fn persisted_event_count(&self) -> Result<i64, OrbitError> {
        self.store.count_v2_audit_events(&V2AuditEventFilter {
            workspace_id: self.workspace_id.clone(),
            run_id: Some(self.run_id.clone()),
            ..Default::default()
        })
    }

    pub fn blob_store(&self) -> &BlobStore {
        &self.blob_store
    }

    fn write_loop_event(&self, event: &LoopAuditEvent) -> Result<(), OrbitError> {
        let event_id = self.next_loop_event_id()?;
        let payload_json = serde_json::to_string(event)
            .map_err(|err| OrbitError::Store(format!("serialize loop audit event: {err}")))?;
        self.store.insert_v2_audit_event(&V2AuditEventInsertParams {
            workspace_id: self.workspace_id.clone(),
            event_id,
            source: "loop_event".to_string(),
            schema_version: 1,
            event_type: loop_event_type(event).to_string(),
            ts: loop_event_ts(event),
            run_id: loop_event_run_id(event).unwrap_or(&self.run_id).to_string(),
            agent_identity: self.agent_identity.clone(),
            parent_event_id: None,
            workspace_path: self.workspace_path.clone(),
            payload_json,
        })
    }

    fn next_loop_event_id(&self) -> Result<String, OrbitError> {
        let mut counter = self
            .loop_event_counter
            .lock()
            .map_err(|err| OrbitError::Store(format!("loop audit mutex poisoned: {err}")))?;
        *counter += 1;
        Ok(format!("loopevt-{}-{:08x}", self.run_id, *counter))
    }
}

impl AuditSink for V2SqliteSink {
    fn emit(&self, event: &LoopAuditEvent) {
        if let Err(err) = self.write_loop_event(event) {
            tracing::warn!("failed to persist loop audit event to sqlite: {err}");
        }
    }

    fn write_blob(&self, content: &[u8]) -> String {
        self.blob_store
            .write(content)
            .unwrap_or_else(|err| format!("error:{err}"))
    }
}

fn loop_event_ts(event: &LoopAuditEvent) -> DateTime<Utc> {
    match event {
        LoopAuditEvent::SessionSpawn { ts, .. }
        | LoopAuditEvent::SessionClose { ts, .. }
        | LoopAuditEvent::HttpRequest { ts, .. }
        | LoopAuditEvent::HttpResponse { ts, .. }
        | LoopAuditEvent::ToolCallRequested { ts, .. }
        | LoopAuditEvent::ToolCallResult { ts, .. }
        | LoopAuditEvent::IterationBoundary { ts, .. }
        | LoopAuditEvent::PolicyDenial { ts, .. } => *ts,
    }
}

fn loop_event_run_id(event: &LoopAuditEvent) -> Option<&str> {
    match event {
        LoopAuditEvent::SessionSpawn { run_id, .. }
        | LoopAuditEvent::SessionClose { run_id, .. }
        | LoopAuditEvent::HttpRequest { run_id, .. }
        | LoopAuditEvent::HttpResponse { run_id, .. }
        | LoopAuditEvent::ToolCallRequested { run_id, .. }
        | LoopAuditEvent::ToolCallResult { run_id, .. }
        | LoopAuditEvent::IterationBoundary { run_id, .. }
        | LoopAuditEvent::PolicyDenial { run_id, .. } => Some(run_id),
    }
}

fn loop_event_type(event: &LoopAuditEvent) -> &'static str {
    match event {
        LoopAuditEvent::SessionSpawn { .. } => "session.spawn",
        LoopAuditEvent::SessionClose { .. } => "session.close",
        LoopAuditEvent::HttpRequest { .. } => "http.request",
        LoopAuditEvent::HttpResponse { .. } => "http.response",
        LoopAuditEvent::ToolCallRequested { .. } => "tool.call.requested",
        LoopAuditEvent::ToolCallResult { .. } => "tool.call.result",
        LoopAuditEvent::IterationBoundary { .. } => "iteration.boundary",
        LoopAuditEvent::PolicyDenial { .. } => "policy.denial",
    }
}
