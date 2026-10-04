//! Workspace-scoped session-log file store.
//!
//! The log is JSON Lines at `<workspace>/.orbit/session-log.jsonl`. Every read
//! and mutation is serialized through one stable advisory-lock sidecar so ID
//! allocation, append, and resolution observe one ordered record stream.

mod persistence;

use std::path::PathBuf;

use orbit_common::OrbitError;

use crate::contracts::{
    SessionLogAppendParams, SessionLogEntry, SessionLogFilter, SessionLogKind,
    SessionLogStoreBackend,
};

/// File-backed session log rooted in one workspace's `.orbit` directory.
#[derive(Debug, Clone)]
pub struct SessionLogStore {
    orbit_dir: PathBuf,
}

impl SessionLogStore {
    pub fn new(orbit_dir: impl Into<PathBuf>) -> Self {
        Self {
            orbit_dir: orbit_dir.into(),
        }
    }

    /// Append one record and allocate its sequential ID atomically with respect
    /// to every other session-log operation.
    pub fn append(&self, params: SessionLogAppendParams) -> Result<SessionLogEntry, OrbitError> {
        persistence::append(&self.orbit_dir, params)
    }

    /// List records from one consistent, lock-protected snapshot.
    pub fn list(&self, filter: SessionLogFilter) -> Result<Vec<SessionLogEntry>, OrbitError> {
        persistence::list(&self.orbit_dir, &filter)
    }

    /// Mark one `check_later` record resolved using a crash-safe replacement.
    pub fn resolve(&self, id: &str) -> Result<SessionLogEntry, OrbitError> {
        persistence::resolve(&self.orbit_dir, id)
    }
}

impl SessionLogStoreBackend for SessionLogStore {
    fn append(&self, params: SessionLogAppendParams) -> Result<SessionLogEntry, OrbitError> {
        Self::append(self, params)
    }

    fn list(&self, filter: SessionLogFilter) -> Result<Vec<SessionLogEntry>, OrbitError> {
        Self::list(self, filter)
    }

    fn resolve(&self, id: &str) -> Result<SessionLogEntry, OrbitError> {
        Self::resolve(self, id)
    }
}
