//! Host answers for an `auto-task:` job step
//! (`docs/design/activity-job/specs/auto-task-step.md` §2-§5): the owner's
//! deterministic observation of one step pass and its keyed admission.

use chrono::{DateTime, Utc};
use orbit_types::workflow::{AutoTaskReceipt, AutoTaskStepRecord};

/// What the engine asks the owner to observe for one step pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoTaskStepRequest {
    pub step_id: String,
    /// The definition the step names.
    pub auto_task: String,
}

/// Whether the step's readiness predicate holds now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoTaskReadiness {
    Ready,
    /// False or unknown; says which clause or fact.
    Waiting(String),
}

/// The owner's deterministic observation of one auto-task step pass. It
/// writes nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoTaskObservation {
    /// The owner's pass time (`now`).
    pub now: DateTime<Utc>,
    /// The durable step record, when the step was activated before.
    pub record: Option<AutoTaskStepRecord>,
    /// Key and digest a fresh activation records.
    pub action_key: String,
    pub input_digest: String,
    /// Why the referenced definition holds admission (missing, disabled,
    /// guard false or unknown), when it does.
    pub definition_hold: Option<String>,
    /// The terminal settlement of the bound task, when it has one.
    pub receipt: Option<AutoTaskReceipt>,
    /// The bound task's raw status while it is still open.
    pub bound_status: Option<String>,
    /// The earliest deadline known now (outer bound or a step expiry whose
    /// time boundary is recoverable). A receipt settled at or before it
    /// still discharges the step.
    pub deadline: Option<DateTime<Utc>>,
    /// Why the step or job expired, when it did.
    pub expired: Option<String>,
    pub readiness: AutoTaskReadiness,
    /// When the owner should observe a waiting step again: its next poll,
    /// never later than `deadline`.
    pub recheck_at: DateTime<Utc>,
}

/// The owner's keyed admission decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoTaskAdmission {
    /// The reservation resolved to a task: minted now, minted earlier
    /// (resolved from its creation receipt), or compatible open work.
    Bound { task_id: String, minted: bool },
    /// The re-checked readiness predicate no longer holds under the fence.
    Waiting(String),
    /// Incompatible open work or a held definition: no task is created.
    Held(String),
    /// The step or job expired: admits nothing.
    Expired(String),
}
