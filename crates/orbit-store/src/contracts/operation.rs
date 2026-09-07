//! Durable operation-mode authority: grants and aggregate recovery ledgers
//! [ORB-11332].
//!
//! Store owns persistence and the atomic invariants (one active grant per
//! workspace, compare-and-set transitions, episode reservation before
//! dispatch). Core decides *whether* to ask; the answers here are facts.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::workflow::{
    OperationGrant, RecoveryEpisodeKind, RecoveryLedger, RecoveryReservation,
};

/// Result of inserting a new grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantInsertOutcome {
    /// The grant is durable.
    Inserted,
    /// The workspace already has an active, unexpired grant; stop it first.
    ActiveGrantExists(String),
}

/// Which transition an operator requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantTransitionKind {
    /// Stop new admissions; admitted work keeps its captured bounds.
    Stop,
    /// Hard revocation: withdraw privileged actions from admitted work too.
    Revoke,
}

/// One requested transition with its compare-and-set expectation.
#[derive(Debug, Clone)]
pub struct GrantTransitionRequest<'a> {
    pub kind: GrantTransitionKind,
    pub actor: &'a str,
    pub reason: Option<&'a str>,
    /// When set, the transition applies only if the persisted revision matches.
    pub expected_revision: Option<u32>,
    pub now: DateTime<Utc>,
}

/// Result of a grant transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantTransitionOutcome {
    /// The transition was written; the grant reflects it.
    Applied(OperationGrant),
    /// The grant was already in a state the transition cannot strengthen.
    Unchanged(OperationGrant),
    /// The caller's `expected_revision` no longer matches.
    RevisionConflict(OperationGrant),
    /// No such grant in this workspace.
    NotFound,
}

/// Budget for one recovery reservation, captured from the admitting grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryBudget {
    pub episodes: u32,
    pub seconds: u64,
}

/// One request to reserve a recovery episode before dispatch.
#[derive(Debug, Clone)]
pub struct RecoveryReserveRequest<'a> {
    pub task_id: &'a str,
    pub run_id: &'a str,
    pub step_id: Option<&'a str>,
    pub kind: RecoveryEpisodeKind,
    pub budget: RecoveryBudget,
    pub now: DateTime<Utc>,
}

/// Grants and recovery ledgers for a workspace, in the host store.
pub trait OperationStoreBackend: Send + Sync {
    /// Insert a grant unless the workspace already has an active one.
    fn operation_grant_insert(
        &self,
        grant: &OperationGrant,
    ) -> Result<GrantInsertOutcome, OrbitError>;

    /// One grant by id.
    fn operation_grant(
        &self,
        workspace_id: &str,
        grant_id: &str,
    ) -> Result<Option<OperationGrant>, OrbitError>;

    /// The workspace's active, unexpired grant, if any.
    fn operation_active_grant(
        &self,
        workspace_id: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<OperationGrant>, OrbitError>;

    /// Recent grants, newest first.
    fn operation_grants(
        &self,
        workspace_id: &str,
        limit: usize,
    ) -> Result<Vec<OperationGrant>, OrbitError>;

    /// Stop or revoke a grant with compare-and-set on its revision.
    fn operation_grant_transition(
        &self,
        workspace_id: &str,
        grant_id: &str,
        request: &GrantTransitionRequest<'_>,
    ) -> Result<GrantTransitionOutcome, OrbitError>;

    /// Reserve one recovery episode against the task's aggregate ledger.
    fn operation_recovery_reserve(
        &self,
        workspace_id: &str,
        request: &RecoveryReserveRequest<'_>,
    ) -> Result<(RecoveryReservation, RecoveryLedger), OrbitError>;

    /// Record the wall time an open episode consumed.
    fn operation_recovery_settle(
        &self,
        workspace_id: &str,
        task_id: &str,
        episode: u32,
        elapsed_seconds: u64,
        now: DateTime<Utc>,
    ) -> Result<RecoveryLedger, OrbitError>;

    /// The task's ledger, if any recovery was ever reserved.
    fn operation_recovery_ledger(
        &self,
        workspace_id: &str,
        task_id: &str,
    ) -> Result<Option<RecoveryLedger>, OrbitError>;
}
