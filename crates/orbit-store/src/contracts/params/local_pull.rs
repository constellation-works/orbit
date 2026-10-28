use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Caller-side durable identity. Constructed by the internal drain from its
/// runtime destination; it is not an authorization-bearing tool payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullDestination {
    pub owner_machine_id: String,
    pub owner_workspace_id: String,
    pub selector: String,
    pub execution_machine_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalPullPhase {
    Requested,
    Claimed,
    Created,
    Bound,
    Launching,
    Launched,
    Settling,
    Settled,
    Idle,
    /// The owner answered this request with a pre-admission refusal and holds
    /// no receipt for it, so no claim exists to settle [ORB-13625]. Terminal,
    /// like `Idle`: the slot returns and the next pass allocates a new ID.
    Refused,
}

/// How much of a drain's leaf capacity is spoken for, across both admission
/// paths [ORB-12617].
///
/// Legacy `task_auto_pipeline` wrappers, the leaf definitions they dispatch,
/// the claimed leaf definitions a pulled claim binds, and pending pull
/// admissions that have no live run yet all compete for the same configured
/// ceiling. Counting them in one place is what keeps a wrapper and the leaf it
/// dispatched from consuming two slots, and what stops a pending admission
/// from consuming none.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DrainLeafOccupancy {
    /// Distinct occupied slots. A wrapper is replaced by its live leaf
    /// descendants rather than counted beside them, and a terminal leaf keeps
    /// its slot until its claim settles.
    pub occupied: usize,
    /// Occupancy per leaf job definition, reported beside the total so an
    /// operator can see which definitions hold the slots. Includes pending
    /// admissions no live run represents yet.
    pub per_pipeline: std::collections::BTreeMap<String, usize>,
    /// Occupied slots outside the requested coordinator's dispatch lineage or
    /// pull requests. Absent when no coordinator was requested.
    pub inherited: Option<usize>,
    /// The part of `occupied` held by live claimed leaves in their
    /// before-landing review [ORB-15194]. Pull admission admits a replacement
    /// beside each, up to the drain's own ceiling of them; the legacy drain
    /// still counts them as full slots.
    pub reviewing: usize,
}

/// Immutable request and binding with a monotone local execution checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalPullAdmission {
    pub destination: PullDestination,
    pub request: crate::contracts::AdmissionRequest,
    pub receipt: Option<crate::contracts::AdmissionReceipt>,
    pub leaf_run_id: Option<String>,
    pub phase: LocalPullPhase,
    pub settlement: Option<crate::contracts::ClaimMutation>,
    /// The owner's refusal: with [`LocalPullPhase::Refused`], of the request;
    /// with [`LocalPullPhase::Settled`], of a settlement the owner could no
    /// longer accept because it had already ended the claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
    /// The owner's latest refusal of this `Settling` admission's settlement
    /// while it still holds the claim [ORB-13979]. Such a refusal is an
    /// answer, not a lost delivery — the owner declares no required
    /// validation commands, say — so it repeats until an operator changes the
    /// owner. Delivery backs off until `retry_after` rather than asking again
    /// on every pass; the settlement itself stays recorded and is delivered
    /// once the owner accepts it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settlement_refusal: Option<SettlementRefusal>,
}

/// The settled claims that make up a pull drain's failure-breaker streak
/// [ORB-15639]: how many of its most recent claims failed in a row, and their
/// failure classes, newest first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PullFailureStreak {
    /// The failure class of each, by wire name; `untyped` for a failure that
    /// carried none.
    pub classes: Vec<String>,
}

impl PullFailureStreak {
    /// Consecutive failed claims.
    #[must_use]
    pub fn failures(&self) -> usize {
        self.classes.len()
    }
}

/// A pending settlement the owner refused while still holding its claim, and
/// when delivery is next attempted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementRefusal {
    /// The owner's refusal, as it answered.
    pub reason: String,
    /// Stable machine-readable refusal code (`delivery_head_changed`,
    /// `policy_denied`, ...), when the owner answered with one. Durable so
    /// later passes can classify the refusal without parsing `reason`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// Consecutive refused deliveries.
    pub refusals: u32,
    pub first_refused_at: DateTime<Utc>,
    pub last_refused_at: DateTime<Utc>,
    /// No automatic pass delivers before this.
    pub retry_after: DateTime<Utc>,
}

impl LocalPullAdmission {
    /// Whether this admission still holds a drain slot: it is neither an
    /// idle poll, a refused request, nor a settled claim.
    #[must_use]
    pub fn holds_capacity(&self) -> bool {
        !matches!(
            self.phase,
            LocalPullPhase::Idle | LocalPullPhase::Settled | LocalPullPhase::Refused
        )
    }
}

/// Each transition commits before the next network or process side effect.
#[derive(Debug, Clone)]
pub enum LocalPullMutation {
    Receive(Box<crate::contracts::AdmissionReceipt>),
    CreateLeaf,
    Bound,
    LaunchIntent,
    Launched,
    Settle(Box<crate::contracts::ClaimMutation>),
    Settled,
    /// Close a still-`Requested` admission the owner refused, after the owner
    /// confirmed it holds no live receipt for this request ID.
    Refuse(String),
    /// Close a `Settling` admission whose settlement the owner refused, after
    /// the owner confirmed it had already ended the claim (revoked, failed or
    /// landed it). The settlement can never be delivered, so the record
    /// settles locally with the refusal, and its slot is released.
    SettleObsolete(String),
    /// Record the owner's refusal of a `Settling` admission's settlement while
    /// it still holds the claim, and when delivery is next attempted. The
    /// admission stays `Settling` with its settlement unchanged.
    DeferSettlement(SettlementRefusal),
}
