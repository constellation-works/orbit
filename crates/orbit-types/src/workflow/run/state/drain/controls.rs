//! Operator controls, waiting tasks, approvals and capacity.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A graceful cancel of a pull drain that is waiting for its launched leaves.
///
/// The drain stops admitting at once and hands its unlaunched claims back to
/// the owner, but stays the same running coordinator until every leaf it
/// launched has finished and settled; only then does it end `cancelled`. A
/// forced cancel does not wait and records nothing here.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DrainCancelRequest {
    pub actor: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub requested_at: DateTime<Utc>,
}

/// Task disposition chosen by an operator cancellation. Kept in the run's
/// durable state before the owner is signalled so whichever process wins run
/// finalization applies the same task outcome and preserves the cancel note.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskCancellationPolicy {
    /// `true` preserves the legacy `blocked` transition; `false` returns the
    /// task to `backlog` so its candidate can be resumed later.
    pub block: bool,
    /// Actor and optional reason captured for the task history entry.
    pub note: String,
}

/// A live operator adjustment to a bounded drain's worker ceiling [ORB-11253].
///
/// The ceiling a drain was submitted with lives in its immutable
/// `initial_input`, which is why raising it used to mean cancelling the
/// coordinator and submitting a replacement. This is the mutable counterpart:
/// the admission path prefers it over the submitted value, so the same run id,
/// deadline, completion policy, and already-dispatched children survive the
/// change.
///
/// `revision` is the compare-and-set handle. Every accepted update increments
/// it, so a caller that read revision *n* and writes with `expected_revision`
/// *n* cannot silently overwrite an update that landed in between.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DrainWorkerLimit {
    /// Live ceiling on concurrently live leaf runs.
    pub max_active_leaf_runs: u32,
    /// The ceiling this update replaced, kept as change evidence.
    pub previous_max_active_leaf_runs: u32,
    /// Accepted-update counter, starting at 1.
    pub revision: u32,
    pub actor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub updated_at: DateTime<Utc>,
}

/// A live operator extension of a bounded drain's window [ORB-15383].
///
/// A drain stamps its deadline once, in its `open_window` step output, and
/// every later window answer echoes that stamp. This is the mutable
/// counterpart, like [`DrainWorkerLimit`] is for the ceiling: the window
/// answer, readiness and upgrade-resume all read the later of the stamp and
/// this deadline, so the run keeps its id, its children and its authority
/// while its window grows. It never shortens a window and never reopens a
/// drain whose admissions were stopped.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DrainWindowExtension {
    /// The deadline in force since this extension.
    pub deadline: DateTime<Utc>,
    /// The deadline it replaced, kept as change evidence.
    pub previous_deadline: DateTime<Utc>,
    /// Accepted-extension counter, starting at 1.
    pub revision: u32,
    pub actor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub extended_at: DateTime<Utc>,
}

/// A backlog task a drain's admission pass left unstarted, and why.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DrainWaitingTask {
    pub task_id: String,
    /// Why the drain could not admit it (`context_lock_conflict`,
    /// `crew_not_allowed`, `host_os_mismatch`, ...); absent for a plain lock
    /// deferral.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The tasks holding what it needs, when known.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_by: Vec<String>,
    /// What clears the wait, when the reason alone does not say: the host OS
    /// a `host_os_mismatch` task waits for, say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// What an `--approve-proposed` drain has done with `proposed` work.
///
/// The approval steps write it on the drain's own run state: the selection
/// step replaces the held view every pass, and the record step in the pass
/// that sees a task-pilot child finish adds the tasks that child approved,
/// closing the child's dispatch so it is counted once. Like `drain_last_pass` it
/// survives terminalization, so `run show` reports a finished drain's
/// approvals and holds.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct DrainApprovalReport {
    pub recorded_at: Option<DateTime<Utc>>,
    /// Proposed tasks this drain moved to `backlog` over its whole window.
    #[serde(default)]
    pub approved_total: u64,
    /// The most recent of those (bounded list).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approved: Vec<String>,
    /// Proposed tasks Orbit automation filed that this drain archived because
    /// task-pilot verified them already fixed by a commit on the base branch.
    #[serde(default)]
    pub closed_total: u64,
    /// The most recent of those (bounded list).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub closed: Vec<String>,
    /// Proposed tasks the latest pass left proposed, with the reason: the
    /// qualification they miss, or the task-pilot classification that held
    /// them (bounded list).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub held: Vec<DrainWaitingTask>,
    /// The full count behind `held`.
    #[serde(default)]
    pub held_total: u64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub held_by_reason: BTreeMap<String, u64>,
}

/// Workspace capacity observed before a local drain's latest admission wave.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DrainCapacity {
    /// Shared slots occupied by live deliveries and unsettled pull admissions.
    pub active_leaf_runs: u64,
    /// Occupied slots outside this coordinator's dispatch lineage or claims.
    pub inherited_leaf_runs: u64,
    /// Effective ceiling, including an operator's live resize.
    pub max_active_leaf_runs: u64,
}
