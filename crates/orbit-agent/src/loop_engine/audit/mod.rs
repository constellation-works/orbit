//! Audit blob sinks for runtime persistence.
//!
//! Payload blobs are redacted before storage.

use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

pub use orbit_common::storage::blob_store::BlobStore;

pub trait AuditSink: Send + Sync {
    fn write_blob(&self, content: &[u8]) -> String;
}

pub struct NullSink;

impl AuditSink for NullSink {
    fn write_blob(&self, _content: &[u8]) -> String {
        String::new()
    }
}

pub struct InMemorySink {
    blobs: Mutex<Vec<(String, Vec<u8>)>>,
    blob_store: BlobStore,
}

impl InMemorySink {
    pub fn new(blob_root: impl Into<PathBuf>) -> Self {
        Self {
            blobs: Mutex::new(Vec::new()),
            blob_store: BlobStore::new(blob_root),
        }
    }

    pub fn blob_store(&self) -> &BlobStore {
        &self.blob_store
    }
}

impl AuditSink for InMemorySink {
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
