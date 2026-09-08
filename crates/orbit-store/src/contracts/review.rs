//! Durable before-PR review evidence: lineage ledgers, certificates, and
//! landing records [ORB-11333].
//!
//! Store owns the atomic invariants: a reviewer start is reserved before a
//! reviewer is launched, settlement is idempotent, certificates are
//! immutable, and a landing record never rewrites the certificate it maps.
//! Core decides when to ask; orbit-automation decides what the answers mean.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::workflow::automation::SourceRevision;
use orbit_types::workflow::{
    ReviewBudget, ReviewCertificate, ReviewLanding, ReviewLedger, ReviewReservation, ReviewVerdict,
};

/// One request to start (or resume) a reviewer for a candidate lineage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewReserveRequest<'a> {
    pub lineage_key: &'a str,
    pub task_ids: &'a [String],
    pub run_id: &'a str,
    pub task_meaning_digest: &'a str,
    pub candidate: &'a SourceRevision,
    /// Captured at admission; the ledger keeps the first budget it saw.
    pub budget: ReviewBudget,
    pub now: DateTime<Utc>,
}

/// The settled outcome of one attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewSettlement<'a> {
    pub lineage_key: &'a str,
    pub attempt_id: &'a str,
    pub verdict: ReviewVerdict,
    pub repair_cycles: u32,
    pub elapsed_seconds: u64,
    pub now: DateTime<Utc>,
}

pub trait ReviewStoreBackend: Send + Sync {
    /// The ledger for one lineage, if any attempt was ever reserved.
    fn review_ledger(
        &self,
        workspace_id: &str,
        lineage_key: &str,
    ) -> Result<Option<ReviewLedger>, OrbitError>;

    /// Reserve a reviewer start. An open attempt for the same candidate and
    /// task meaning is resumed rather than charged again; a different
    /// candidate settles the open attempt as incomplete, charging elapsed
    /// wall time from `started_at` to `now` once, then applies the captured
    /// budget to the new start.
    fn review_reserve(
        &self,
        workspace_id: &str,
        request: &ReviewReserveRequest<'_>,
    ) -> Result<(ReviewReservation, ReviewLedger), OrbitError>;

    /// Settle an attempt. Replaying the same settlement changes nothing.
    fn review_settle(
        &self,
        workspace_id: &str,
        settlement: &ReviewSettlement<'_>,
    ) -> Result<ReviewLedger, OrbitError>;

    /// Record an immutable certificate. Re-recording identical bytes is a
    /// no-op; a changed certificate under the same attempt is refused.
    fn review_certificate_record(
        &self,
        workspace_id: &str,
        certificate: &ReviewCertificate,
    ) -> Result<(), OrbitError>;

    /// One certificate by attempt id.
    fn review_certificate(
        &self,
        workspace_id: &str,
        attempt_id: &str,
    ) -> Result<Option<ReviewCertificate>, OrbitError>;

    /// Passed certificates whose final candidate tree matches, newest first.
    fn review_certificates_for_tree(
        &self,
        repository: &str,
        final_candidate_tree: &str,
        limit: usize,
    ) -> Result<Vec<ReviewCertificate>, OrbitError>;

    /// Record how a certificate's candidate actually landed.
    fn review_landing_record(&self, landing: &ReviewLanding) -> Result<(), OrbitError>;

    /// Landing records for one certificate, oldest first.
    fn review_landings(&self, attempt_id: &str) -> Result<Vec<ReviewLanding>, OrbitError>;
}
