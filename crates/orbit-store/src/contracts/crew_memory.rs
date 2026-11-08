//! Crew memory in the owner store: append-only revisions of each memory
//! space and the audit of every write, refusal, reset and copy. The store
//! assigns revision numbers and keeps rows append-only; what a revision may
//! hold, who may write it and how it renders are core's.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;

/// One immutable revision of a memory space. The newest revision of a key is
/// the space's current content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrewMemoryRevisionRow {
    pub workspace_id: String,
    /// A model id, or `global`.
    pub memory_key: String,
    /// 1 for a key's first revision; assigned by the store.
    pub revision: u32,
    /// The space's files as a JSON array of `{path, text}` objects.
    pub files_json: String,
    /// `sha256:<hex>` over the files, as core computes it.
    pub content_digest: String,
    /// The cited evidence task ids as a JSON array of strings.
    pub evidence_json: String,
    /// `operator` or `run`.
    pub writer_kind: String,
    /// The operator label, or the writing run's agent.
    pub writer_actor: String,
    pub writer_run_id: Option<String>,
    pub writer_crew: Option<String>,
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// One row of the append-only crew memory audit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrewMemoryEventRow {
    pub workspace_id: String,
    pub memory_key: String,
    pub at: DateTime<Utc>,
    /// `write_refused`, `revision_written`, `reset` or `copied`; M4 adds the
    /// proposal actions.
    pub action: String,
    pub actor: String,
    /// The revision the action wrote, when it wrote one.
    pub revision: Option<u32>,
    /// Action-specific detail as a JSON object.
    pub detail_json: String,
}

pub trait CrewMemoryStoreBackend: Send + Sync {
    /// Append `revision` as its key's next revision and `event` with that
    /// number, in one immediate transaction. The `revision` fields of both
    /// inputs are ignored. Returns the stored revision.
    fn append_crew_memory_revision(
        &self,
        revision: &CrewMemoryRevisionRow,
        event: &CrewMemoryEventRow,
    ) -> Result<CrewMemoryRevisionRow, OrbitError>;

    /// Append one audit event that writes no revision, such as a refusal.
    fn append_crew_memory_event(&self, event: &CrewMemoryEventRow) -> Result<(), OrbitError>;

    /// One revision of `memory_key`: `revision`, or the newest when `None`.
    fn crew_memory_revision(
        &self,
        workspace_id: &str,
        memory_key: &str,
        revision: Option<u32>,
    ) -> Result<Option<CrewMemoryRevisionRow>, OrbitError>;

    /// Every revision of `memory_key`, newest first.
    fn crew_memory_revisions(
        &self,
        workspace_id: &str,
        memory_key: &str,
    ) -> Result<Vec<CrewMemoryRevisionRow>, OrbitError>;

    /// The newest revision of every key in the workspace, by key.
    fn crew_memory_current(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<CrewMemoryRevisionRow>, OrbitError>;

    /// The audit events of `memory_key`, oldest first.
    fn crew_memory_events(
        &self,
        workspace_id: &str,
        memory_key: &str,
    ) -> Result<Vec<CrewMemoryEventRow>, OrbitError>;
}
