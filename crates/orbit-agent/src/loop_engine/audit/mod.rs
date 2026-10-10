//! Structured audit contracts used by the runtime's persistent sinks.
//!
//! Session, HTTP, tool and policy event variants are retained for historical
//! rows in `v2_audit_events`. Events carry hashes pointing to payloads redacted
//! at write time in a separate content-addressed [`BlobStore`].
//!
//! Persistent audit storage is owned by the runtime layer. Tests use
//! [`InMemorySink`], callers with no need for persistence use [`NullSink`].

use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// Keep the engine's existing audit imports stable. New code can use the
// common crate's storage and redaction mechanisms directly.
pub use orbit_common::security::redaction::PatternRedactor as RedactionMiddleware;
pub use orbit_common::storage::blob_store::BlobStore;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageSnapshot {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event_kind", rename_all = "snake_case")]
pub enum LoopAuditEvent {
    SessionSpawn {
        ts: DateTime<Utc>,
        run_id: String,
        session_id: String,
        provider: String,
        model: String,
        task_id: Option<String>,
        audit_tag: Option<String>,
    },
    SessionClose {
        ts: DateTime<Utc>,
        run_id: String,
        session_id: String,
        reason: String,
    },
    HttpRequest {
        ts: DateTime<Utc>,
        run_id: String,
        session_id: String,
        iteration: u32,
        provider: String,
        model: String,
        endpoint: String,
        body_sha256: String,
    },
    HttpResponse {
        ts: DateTime<Utc>,
        run_id: String,
        session_id: String,
        iteration: u32,
        http_status: u16,
        stop_reason: String,
        usage: UsageSnapshot,
        body_sha256: String,
    },
    ToolCallRequested {
        ts: DateTime<Utc>,
        run_id: String,
        session_id: String,
        iteration: u32,
        tool_name: String,
        tool_use_id: String,
        input_sha256: String,
    },
    ToolCallResult {
        ts: DateTime<Utc>,
        run_id: String,
        session_id: String,
        iteration: u32,
        tool_name: String,
        tool_use_id: String,
        outcome: String,
        output_sha256: String,
        #[serde(deserialize_with = "deserialize_duration_ms")]
        duration_ms: u128,
    },
    IterationBoundary {
        ts: DateTime<Utc>,
        run_id: String,
        session_id: String,
        iteration: u32,
        continues: bool,
    },
    PolicyDenial {
        ts: DateTime<Utc>,
        run_id: String,
        session_id: String,
        iteration: u32,
        tool_name: String,
        reason: String,
    },
}

// Serde's internally tagged buffer does not support deserialize_u128. Persisted
// millisecond durations fit u64; widen on read without changing serialization.
fn deserialize_duration_ms<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<u128, D::Error> {
    u64::deserialize(deserializer).map(u128::from)
}

pub trait AuditSink: Send + Sync {
    fn emit(&self, event: &LoopAuditEvent);
    fn write_blob(&self, content: &[u8]) -> String;
}

pub struct NullSink;

impl AuditSink for NullSink {
    fn emit(&self, _event: &LoopAuditEvent) {}
    fn write_blob(&self, _content: &[u8]) -> String {
        String::new()
    }
}

pub struct InMemorySink {
    events: Mutex<Vec<LoopAuditEvent>>,
    blobs: Mutex<Vec<(String, Vec<u8>)>>,
    blob_store: BlobStore,
}

impl InMemorySink {
    pub fn new(blob_root: impl Into<PathBuf>) -> Self {
        Self {
            events: Mutex::new(Vec::new()),
            blobs: Mutex::new(Vec::new()),
            blob_store: BlobStore::new(blob_root),
        }
    }

    pub fn events(&self) -> Vec<LoopAuditEvent> {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn blob_store(&self) -> &BlobStore {
        &self.blob_store
    }
}

impl AuditSink for InMemorySink {
    fn emit(&self, event: &LoopAuditEvent) {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(event.clone());
    }
    fn write_blob(&self, content: &[u8]) -> String {
        let hash = self
            .blob_store
            .write(content)
            .unwrap_or_else(|err| format!("error:{err}"));
        let stored = self.blob_store.redact_for_storage(content);
        self.blobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((hash.clone(), stored));
        hash
    }
}
