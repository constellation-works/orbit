//! Durable before-PR review evidence: lineage ledgers, certificates, and
//! landing records [ORB-11333].
//!
//! Core applies review rules inside host-store transactions.

use chrono::{DateTime, Utc};
use orbit_types::workflow::automation::SourceRevision;
use orbit_types::workflow::{ReviewBudget, ReviewVerdict, ReviewerInvocationEvent};

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
