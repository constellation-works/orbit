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
    ReviewBudget, ReviewCertificate, ReviewLanding, ReviewLedger, ReviewReconciliation,
    ReviewReservation, ReviewVerdict, ReviewerInvocationEvent,
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

/// The settled outcome of one attempt. The charge is the attempt's own
/// recorded reviewer runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewSettlement<'a> {
    pub lineage_key: &'a str,
    pub attempt_id: &'a str,
    pub verdict: ReviewVerdict,
    pub now: DateTime<Utc>,
}

/// Close an attempt that ended without a reviewer verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewRelease<'a> {
    pub lineage_key: &'a str,
    pub attempt_id: &'a str,
    /// The latest instant a reviewer still running for the attempt can have
    /// run until; see `ReviewAttempt::reviewer_runtime_at`.
    pub bound: DateTime<Utc>,
    pub now: DateTime<Utc>,
}

/// A reviewer invocation of `run_id` starting or ending for an attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewInvocationRecord<'a> {
    pub lineage_key: &'a str,
    pub attempt_id: &'a str,
    pub run_id: &'a str,
    pub event: ReviewerInvocationEvent,
    pub now: DateTime<Utc>,
}

/// Reset one explicitly selected lineage. Authorization belongs to Core;
/// Store atomically retains the old attempts and the operator decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewResetRequest<'a> {
    pub lineage_key: &'a str,
    pub task_id: &'a str,
    pub reason: &'a str,
    pub actor: &'a str,
    /// None retains the captured budget; Some explicitly adopts a new one.
    pub budget: Option<ReviewBudget>,
    pub now: DateTime<Utc>,
}

pub trait ReviewStoreBackend: Send + Sync {
    /// Close the open attempt and start a fresh budget, preserving all history.
    fn review_reset(
        &self,
        workspace_id: &str,
        request: &ReviewResetRequest<'_>,
    ) -> Result<ReviewLedger, OrbitError>;

    /// The ledger for one lineage, if any attempt was ever reserved.
    fn review_ledger(
        &self,
        workspace_id: &str,
        lineage_key: &str,
    ) -> Result<Option<ReviewLedger>, OrbitError>;

    /// Reserve a reviewer start. An open attempt for the same candidate and
    /// task meaning is resumed rather than charged again; a different
    /// candidate releases the open attempt as incomplete, charging its
    /// reviewer runtime once, then applies the captured budget to the new
    /// start.
    fn review_reserve(
        &self,
        workspace_id: &str,
        request: &ReviewReserveRequest<'_>,
    ) -> Result<(ReviewReservation, ReviewLedger), OrbitError>;

    /// Settle an attempt. Replaying the same settlement changes nothing; a
    /// released attempt is settled again with the verdict, its provisional
    /// charge replaced.
    fn review_settle(
        &self,
        workspace_id: &str,
        settlement: &ReviewSettlement<'_>,
    ) -> Result<ReviewLedger, OrbitError>;

    /// Release an attempt whose reviewer step failed or whose run ended:
    /// an open (or already released) attempt is settled `incomplete`,
    /// charged its reviewer runtime, so no failed attempt stays open. An
    /// attempt settled with a verdict is left unchanged.
    fn review_release(
        &self,
        workspace_id: &str,
        release: &ReviewRelease<'_>,
    ) -> Result<ReviewLedger, OrbitError>;

    /// Release every attempt `run_id` still holds — open under it, or with
    /// its reviewer running — as the run terminates at `finished_at`.
    /// Returns the ledgers that changed.
    fn review_release_run(
        &self,
        workspace_id: &str,
        run_id: &str,
        finished_at: DateTime<Utc>,
    ) -> Result<Vec<ReviewLedger>, OrbitError>;

    /// Record a reviewer invocation starting or finishing for an attempt, so
    /// the lineage is charged reviewer process runtime only. An attempt
    /// settled with a verdict records nothing.
    fn review_record_invocation(
        &self,
        workspace_id: &str,
        record: &ReviewInvocationRecord<'_>,
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

    /// Insert a reconciliation, or return the one already recorded for its
    /// task and request key. A key reused for a different binding is refused.
    fn review_reconciliation_open(
        &self,
        workspace_id: &str,
        record: &ReviewReconciliation,
    ) -> Result<ReviewReconciliation, OrbitError>;

    /// One reconciliation by id.
    fn review_reconciliation(
        &self,
        workspace_id: &str,
        reconciliation_id: &str,
    ) -> Result<Option<ReviewReconciliation>, OrbitError>;

    /// Every reconciliation of a task, newest first.
    fn review_reconciliations_for_task(
        &self,
        workspace_id: &str,
        task_id: &str,
    ) -> Result<Vec<ReviewReconciliation>, OrbitError>;

    /// Replace a reconciliation, fencing on the revision the caller read. The
    /// stored revision is advanced and returned in the record.
    fn review_reconciliation_update(
        &self,
        workspace_id: &str,
        record: &ReviewReconciliation,
    ) -> Result<ReviewReconciliation, OrbitError>;
}
