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

/// One proposal for the global memory, opened by a dreaming run (slice M3)
/// and decided by an operator (slice M4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrewMemoryProposalRow {
    pub workspace_id: String,
    pub id: String,
    /// The model key whose dreaming run proposed it.
    pub proposing_key: String,
    pub proposing_run_id: Option<String>,
    pub text: String,
    /// The cited evidence task ids as a JSON array of strings.
    pub evidence_json: String,
    /// `open`, `accepted`, `rejected` or `withdrawn`.
    pub status: String,
    pub created_at: DateTime<Utc>,
}

/// What [`CrewMemoryStoreBackend::open_crew_memory_proposal`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrewMemoryProposalOutcome {
    Opened,
    /// The proposing run already holds `existing` proposals, at or over the
    /// cap; nothing was written.
    CapReached {
        existing: u32,
    },
}

/// An operator's decision on an open global-memory proposal.
pub struct CrewMemoryProposalDecision {
    pub workspace_id: String,
    pub id: String,
    /// `accepted` or `rejected`.
    pub status: String,
    pub actor: String,
    pub at: DateTime<Utc>,
    pub reason: String,
}

/// One row of the append-only crew memory audit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrewMemoryEventRow {
    pub workspace_id: String,
    pub memory_key: String,
    pub at: DateTime<Utc>,
    /// `write_refused`, `revision_written`, `reset` or `copied`; dreaming
    /// (M3) adds `dream_minted`, `dream_skipped`, `no_revision`,
    /// `proposal_opened` and `propose_refused`, and M4 the proposal
    /// decisions.
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

    /// Insert `proposal` and `event` in one immediate transaction, unless
    /// the proposing run already holds `max_per_run` proposals in the
    /// workspace, counted inside the same transaction.
    fn open_crew_memory_proposal(
        &self,
        proposal: &CrewMemoryProposalRow,
        event: &CrewMemoryEventRow,
        max_per_run: u32,
    ) -> Result<CrewMemoryProposalOutcome, OrbitError>;

    /// Open proposals, oldest first (then id). Core expires old proposals
    /// before reading this list.
    fn crew_memory_open_proposals(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<CrewMemoryProposalRow>, OrbitError>;

    /// Withdraw open proposals created before `before`, recording each
    /// expiry and its decision fields atomically with its audit event.
    fn expire_crew_memory_proposals(
        &self,
        workspace_id: &str,
        before: DateTime<Utc>,
        at: DateTime<Utc>,
    ) -> Result<usize, OrbitError>;

    /// Decide an open proposal and append its audit event in one immediate
    /// transaction. An acceptance also appends its global revision. The
    /// tuple's number is the expected current global revision (0 if empty);
    /// refuse a stale read rather than overwrite intervening operator work.
    fn decide_crew_memory_proposal(
        &self,
        decision: &CrewMemoryProposalDecision,
        event: &CrewMemoryEventRow,
        revision: Option<(&CrewMemoryRevisionRow, u32)>,
    ) -> Result<Option<CrewMemoryRevisionRow>, OrbitError>;

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
