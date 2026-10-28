use serde::{Deserialize, Serialize};

use super::{ExpiredTaskReservation, TaskLockConflict, TaskReservationReleaseReason};

/// Whether a path lease blocks overlapping work. A task reservation is
/// always `hard`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseMode {
    /// Recorded and visible, but blocks nothing.
    Advisory,
    /// Blocks overlapping admission and reservations like a task reservation.
    Hard,
}

impl LeaseMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Advisory => "advisory",
            Self::Hard => "hard",
        }
    }

    /// An unknown stored mode reads as `hard`, so a value this binary does
    /// not know over-blocks rather than letting an overlap through.
    pub(crate) fn parse(raw: &str) -> Self {
        match raw {
            "advisory" => Self::Advisory,
            _ => Self::Hard,
        }
    }
}

/// Who a lease's principal is: a person or an agent acting for one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeasePrincipalClass {
    Human,
    Agent,
}

impl LeasePrincipalClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Agent => "agent",
        }
    }

    pub(crate) fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "human" => Ok(Self::Human),
            "agent" => Ok(Self::Agent),
            other => Err(format!("unknown lease principal class '{other}'")),
        }
    }
}

/// A lease's priority in the lease queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeasePriority {
    Low,
    Normal,
    High,
}

impl LeasePriority {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Normal => "normal",
            Self::High => "high",
        }
    }

    pub(crate) fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "low" => Ok(Self::Low),
            "normal" => Ok(Self::Normal),
            "high" => Ok(Self::High),
            other => Err(format!("unknown lease priority '{other}'")),
        }
    }
}

/// Record a path lease. An advisory one is granted unless
/// `active_lease_limit` refuses it. A hard one is
/// granted when its selectors overlap no active task reservation, hard lease
/// or `task_conflicts` entry and no queued request or expansion ranked ahead
/// of it wants them; otherwise it queues, or with `wait = false` is refused.
/// Task context locks and path policy are the runtime's to apply first.
#[derive(Debug, Clone)]
pub struct LeaseInsertParams {
    pub workspace_orbit_dir: String,
    pub workspace_id: Option<String>,
    /// Canonical `file:` and `dir:` selectors.
    pub selectors: Vec<String>,
    pub mode: LeaseMode,
    pub priority: LeasePriority,
    pub tags: Vec<String>,
    pub principal: String,
    pub principal_class: LeasePrincipalClass,
    /// The calling machine, for attribution.
    pub machine_id: Option<String>,
    /// The base branch the leased work is against.
    pub base_branch: Option<String>,
    /// The branch or pull request whose landing releases the lease.
    pub bound_branch: Option<String>,
    pub actor: String,
    pub ttl_seconds: u32,
    /// The lease token a listener request authenticated with; `None` for a
    /// lease requested over SSH or the dashboard.
    pub token_id: Option<String>,
    /// Queue a blocked hard request instead of refusing it.
    pub wait: bool,
    /// The task context locks the runtime found on `selectors`. A bounded
    /// caller may pass only those its answer could show, as long as every
    /// blocked selector keeps at least one.
    pub task_conflicts: Vec<TaskLockConflict>,
    /// `lease.aging_minutes`: queue age after which an entry moves up one
    /// priority level.
    pub aging_minutes: u32,
    /// The bytes the answer has left for `conflicts` and then `queued_ahead`,
    /// each entry costing its JSON plus one. Every holder still decides
    /// whether the lease is blocked, but only the prefix that fits is kept,
    /// returned and written to the `queued` notice. Lease-token requests set
    /// it; `None` (SSH and the dashboard) keeps every holder.
    pub holder_list_bytes: Option<usize>,
    /// Refuse the request, writing nothing, while `principal` already has
    /// this many active (granted or queued) leases in the workspace, whatever
    /// token or surface recorded them. Lease-token requests set it; `None`
    /// (SSH and the dashboard) is unlimited.
    pub active_lease_limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseInsertResult {
    /// The recorded lease, granted or queued; `None` when a hard lease was
    /// refused.
    pub lease: Option<LeaseRecord>,
    /// The holders blocking a queued or refused hard lease, by requested
    /// selector, then task locks, reservations and hard leases, then id.
    pub conflicts: Vec<TaskLockConflict>,
    /// Queued requests and expansions ranked ahead that want the same paths,
    /// in queue order.
    pub queued_ahead: Vec<String>,
    /// The 1-based queue position of a queued lease.
    pub queue_position: Option<u32>,
    pub expired_leases: Vec<ExpiredTaskReservation>,
    /// The principal's active lease count when `active_lease_limit` refused
    /// the request; nothing was written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_lease_limit_reached: Option<u32>,
    /// `holder_list_bytes` cut `conflicts` or `queued_ahead` short.
    #[serde(default)]
    pub holders_truncated: bool,
}

/// What a queued lease waits for: a whole request (it holds nothing yet) or
/// an expansion (it keeps what it was granted).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseQueueKind {
    Request,
    Expansion,
}

impl LeaseQueueKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Expansion => "expansion",
        }
    }

    /// An unknown stored kind reads as a request, which holds nothing until
    /// granted, so it can never block more than it was granted.
    pub(crate) fn parse(raw: &str) -> Self {
        match raw {
            "expansion" => Self::Expansion,
            _ => Self::Request,
        }
    }
}

/// Add selectors to a granted lease. Free ones are added at once; on a hard
/// lease, blocked ones queue while the lease keeps what it holds.
#[derive(Debug, Clone)]
pub struct LeaseExpandParams {
    pub workspace_orbit_dir: String,
    pub workspace_id: Option<String>,
    pub lease_id: String,
    /// Canonical selectors the lease does not already cover, after path
    /// policy.
    pub selectors: Vec<String>,
    /// The task context locks the runtime found on `selectors`. A bounded
    /// caller may pass only those its answer could show, as long as every
    /// blocked selector keeps at least one.
    pub task_conflicts: Vec<TaskLockConflict>,
    pub aging_minutes: u32,
    /// The bytes the answer has left for `conflicts`, as for
    /// [`LeaseInsertParams::holder_list_bytes`]: every holder decides which
    /// selectors wait, but only the prefix that fits is returned and written
    /// to the `queued` notice. `None` keeps every holder.
    pub holder_list_bytes: Option<usize>,
    /// Refuse the expansion, writing nothing, when the lease's held and
    /// waiting selectors plus the new ones would exceed this. Lease-token
    /// expansions set it; `None` is unlimited.
    pub max_selectors: Option<u32>,
    /// Maximum serialized size of this expansion's `deadlock` answer value.
    /// The runtime sets it to four bytes (JSON `null` in the fixed answer)
    /// plus the bytes left for `conflicts` after that answer and the
    /// transport margin. `None` (SSH and the dashboard) does not bound it.
    /// When this lease is the victim and the complete cycle is larger, the
    /// transaction writes nothing. When it fits, the cycle is kept whole and
    /// `conflicts` keeps only the prefix that still fits beside it.
    pub deadlock_budget_bytes: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseExpandResult {
    /// The lease after expansion; `None` when it is unknown, ended or still a
    /// queued request.
    pub lease: Option<LeaseRecord>,
    /// Selectors granted at once.
    pub added: Vec<String>,
    /// Selectors now waiting in the queue.
    pub queued: Vec<String>,
    /// The holders blocking the queued selectors, ordered as
    /// [`LeaseInsertResult::conflicts`].
    pub conflicts: Vec<TaskLockConflict>,
    /// The 1-based queue position of the waiting expansion.
    pub queue_position: Option<u32>,
    /// Waiting expansions refused to break a deadlock, this lease's included.
    pub deadlocks: Vec<LeaseDeadlock>,
    pub expired_leases: Vec<ExpiredTaskReservation>,
    /// `max_selectors` refused the expansion; the lease is unchanged.
    #[serde(default)]
    pub selector_limit_exceeded: bool,
    /// `holder_list_bytes` cut `conflicts` short.
    #[serde(default)]
    pub holders_truncated: bool,
}

/// A waiting expansion refused because it closed a cycle of leases each
/// waiting on another's held paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseDeadlock {
    pub lease_id: String,
    /// The selectors whose expansion was refused.
    pub selectors: Vec<String>,
    /// Every lease in the cycle, the refused one included.
    pub cycle: Vec<String>,
}

/// Grant waiting requests and expansions, in queue order, whose paths are now
/// free.
#[derive(Debug, Clone)]
pub struct LeaseQueueGrantParams {
    pub workspace_orbit_dir: String,
    pub workspace_id: Option<String>,
    /// Task context locks on each waiting lease's queued selectors, keyed by
    /// lease id. A waiting lease missing from the map stays queued.
    pub task_conflicts: std::collections::BTreeMap<String, Vec<TaskLockConflict>>,
    pub aging_minutes: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseQueueGrantResult {
    /// What the pass granted, in queue order.
    pub granted: Vec<LeaseQueueGrant>,
    /// Waiting expansions refused because a grant in this pass closed a
    /// deadlock cycle.
    pub deadlocks: Vec<LeaseDeadlock>,
    pub expired_leases: Vec<ExpiredTaskReservation>,
}

/// One queued request or expansion (or part of one) granted from the queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseQueueGrant {
    pub lease_id: String,
    pub kind: LeaseQueueKind,
    /// The selectors granted by this pass.
    pub selectors: Vec<String>,
    /// Selectors of an expansion still waiting.
    pub still_queued: Vec<String>,
}

/// One path lease as recorded, active or ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseRecord {
    pub lease_id: String,
    /// Newest notice acknowledged by this holder; reads never advance it.
    #[serde(default)]
    pub notice_cursor: u64,
    pub workspace_id: Option<String>,
    pub selectors: Vec<String>,
    pub mode: LeaseMode,
    pub priority: LeasePriority,
    pub tags: Vec<String>,
    pub principal: String,
    pub principal_class: LeasePrincipalClass,
    pub machine_id: Option<String>,
    pub base_branch: Option<String>,
    pub bound_branch: Option<String>,
    pub actor: String,
    pub created_at: String,
    pub expires_at: String,
    /// The last grant or renewal.
    pub heartbeat_at: String,
    pub released_at: Option<String>,
    /// `ttl_expired` once lazy expiry has ended the lease.
    pub release_reason: Option<TaskReservationReleaseReason>,
    /// The lease token the request authenticated with, when it came through
    /// the lease intake socket.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_id: Option<String>,
    /// Selectors a queued request or expansion waits for; never held.
    #[serde(default)]
    pub queued_selectors: Vec<String>,
    /// When the waiting request or expansion entered the queue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued_at: Option<String>,
    /// What is waiting, when anything is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_kind: Option<LeaseQueueKind>,
}

/// Move an active lease's `expires_at` to `ttl_seconds` from now.
#[derive(Debug, Clone)]
pub struct LeaseRenewParams {
    pub workspace_orbit_dir: String,
    pub workspace_id: Option<String>,
    pub lease_id: String,
    pub ttl_seconds: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseRenewResult {
    /// The renewed lease; `None` when it is unknown, released or expired.
    pub lease: Option<LeaseRecord>,
    pub expired_leases: Vec<ExpiredTaskReservation>,
}

#[derive(Debug, Clone)]
pub struct LeaseReleaseParams {
    pub workspace_orbit_dir: String,
    pub workspace_id: Option<String>,
    pub lease_id: String,
    pub release_reason: TaskReservationReleaseReason,
    pub release_metadata_json: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseReleaseResult {
    /// The released lease; `None` when it is unknown or already ended.
    pub lease: Option<LeaseRecord>,
    pub expired_leases: Vec<ExpiredTaskReservation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseListResult {
    /// Active leases, oldest first.
    pub leases: Vec<LeaseRecord>,
    pub expired_leases: Vec<ExpiredTaskReservation>,
}
