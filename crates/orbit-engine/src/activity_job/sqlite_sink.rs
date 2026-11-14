use std::path::{Path, PathBuf};

use orbit_agent::loop_engine::audit::{AuditSink, BlobStore};
use orbit_common::OrbitError;
use orbit_common::storage::blob_sweep;
use orbit_store::contracts::{V2AuditEventFilter, V2AuditEventInsertParams, V2AuditStoreBackend};
use orbit_types::workflow::activity_job::V2AuditEvent;

pub struct V2SqliteSink {
    store: std::sync::Arc<dyn V2AuditStoreBackend>,
    workspace_id: String,
    run_id: String,
    blob_store: BlobStore,
}

impl V2SqliteSink {
    pub fn new(
        store: std::sync::Arc<dyn V2AuditStoreBackend>,
        workspace_id: impl Into<String>,
        run_id: impl Into<String>,
        blob_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            store,
            workspace_id: workspace_id.into(),
            run_id: run_id.into(),
            blob_store: BlobStore::new(blob_root),
        }
    }

    pub fn for_audit_root(
        store: std::sync::Arc<dyn V2AuditStoreBackend>,
        workspace_id: impl Into<String>,
        run_id: impl Into<String>,
        audit_root: &Path,
    ) -> Self {
        let mut sink = Self::new(store, workspace_id, run_id, audit_root.join("blobs"));
        // The audit root's blobs are swept by `orbit gc audit`: mark each
        // blob pending until the row that names it is written.
        sink.blob_store = sink
            .blob_store
            .with_pending_root(blob_sweep::pending_root(audit_root));
        sink
    }

    pub fn write_envelope(&self, event: &V2AuditEvent) -> Result<(), OrbitError> {
        let payload_json = serde_json::to_string(event)
            .map_err(|err| OrbitError::Store(format!("serialize v2 audit event: {err}")))?;
        self.store
            .insert_v2_audit_event(&V2AuditEventInsertParams {
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
                payload_json: payload_json.clone(),
            })?;
        self.blob_store.clear_published(&payload_json);
        Ok(())
    }

    /// Every persisted envelope event for this run, oldest first. Backs the
    /// cross-crate `V2AuditWriter::events_snapshot` path, so it must not take
    /// the store's default 1000-row newest-first window: long provider runs
    /// emit progress rows that would push early recovery evidence out of it.
    pub(crate) fn snapshot_events(&self) -> Result<Vec<V2AuditEvent>, OrbitError> {
        let rows = self.store.list_v2_audit_events(&V2AuditEventFilter {
            workspace_id: self.workspace_id.clone(),
            run_id: Some(self.run_id.clone()),
            source: Some("v2_envelope".to_string()),
            limit: Some(usize::MAX),
            oldest_first: true,
            ..Default::default()
        })?;
        rows.into_iter()
            .map(|row| {
                serde_json::from_str(&row.payload_json)
                    .map_err(|error| OrbitError::Store(format!("decode v2 audit event: {error}")))
            })
            .collect()
    }

    pub fn blob_store(&self) -> &BlobStore {
        &self.blob_store
    }
}

/// What [`AuditSink::write_blob`] returns in place of a ref when the blob
/// could not be stored. [`V2AuditWriter::write_blob`] turns it into a counted
/// audit failure, so it never reaches an event as a ref.
///
/// [`V2AuditWriter::write_blob`]: super::audit_writer::V2AuditWriter::write_blob
pub(crate) const BLOB_WRITE_ERROR_PREFIX: &str = "error:";

impl AuditSink for V2SqliteSink {
    fn write_blob(&self, content: &[u8]) -> String {
        self.blob_store
            .write(content)
            .unwrap_or_else(|err| format!("{BLOB_WRITE_ERROR_PREFIX}{err}"))
    }
}
