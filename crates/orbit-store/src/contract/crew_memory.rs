//! Crew memory in the owner store: append-only revisions of each memory
//! space and the audit of every write, refusal, reset and copy. The store
//! keeps rows append-only; revision numbering, what a revision may hold, who
//! may write it and how it renders are core's.

use chrono::{DateTime, Utc};

/// One immutable revision of a memory space. The newest revision of a key is
/// the space's current content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrewMemoryRevisionRow {
    pub workspace_id: String,
    /// A model id, or `global`.
    pub memory_key: String,
    /// 1 for a key's first revision; assigned by the writing operation.
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

/// A decided proposal: accepted, rejected, or withdrawn on expiry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrewMemoryClosedProposalRow {
    pub proposal: CrewMemoryProposalRow,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    pub decision_reason: Option<String>,
}

/// What opening a global-memory proposal did.
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
