//! What a job's gate needs from backlog admission [ORB-17938].
//!
//! A gate refused an ordinary reservation may still admit a resumed
//! published delivery, and a starved gate names the execution claims that
//! hold its blockers. `admission` owns those rules, and it also drives job
//! runs, so a direct call from here would close a cycle. This port is the
//! way in: `admission` implements it on [`OrbitRuntime`](crate::OrbitRuntime),
//! so composing the runtime binds it.

use std::collections::BTreeSet;

use orbit_common::OrbitError;
use orbit_store::contracts::TaskLockConflict;
use orbit_tools::ReservationOwnerContext;
use orbit_types::task::Task;
use serde_json::Value;

use crate::application::contracts::PublishedDelivery;
use crate::runtime::task::locks::ReservationOutcome;

/// The run-input key an operator's explicit ship sets, so its listing step
/// skips the readiness gates and its gate the lock waits the operator
/// overrode [ORB-25082]. Drain children and other callers carrying task ids
/// never set it.
pub(crate) const OPERATOR_READINESS_OVERRIDE_INPUT: &str = "operator_readiness_override";

/// Admission decisions a job's gate calls, implemented by `admission`.
pub(in crate::application) trait JobAdmissionPort {
    /// The reservation a gate takes for a resumed published delivery the
    /// ordinary reservation in `denied` refused, with the publication it
    /// carries, or `None` when `input` is not such a delivery.
    fn published_delivery_reservation(
        &self,
        input: &Value,
        denied: &ReservationOutcome,
        agent: Option<String>,
        model: Option<String>,
        owner: Option<ReservationOwnerContext>,
    ) -> Result<Option<(ReservationOutcome, PublishedDelivery)>, OrbitError>;

    /// The live execution claims holding `task_ids`, as readiness reports
    /// them, and one line naming them, or `None` when no claim holds any.
    fn execution_claims_holding(&self, task_ids: &BTreeSet<String>)
    -> (Vec<Value>, Option<String>);

    /// The drain readiness gates `task` fails now, which an operator's
    /// explicit ship overrides, in a stable order. `schedule_held` is whether
    /// its `not_before` still holds it.
    fn operator_overridden_gates(&self, task: &Task, schedule_held: bool) -> Vec<String>;

    /// The `context_lock_conflict` gate an operator's ship overrides at its
    /// gate, naming the holders of `conflicts`.
    fn lock_conflict_gate(&self, conflicts: &[TaskLockConflict]) -> String;

    /// Record that `actor`'s explicit ship of `task_id` overrode `gates`.
    fn record_readiness_override(
        &self,
        task_id: &str,
        gates: &[String],
        actor: &str,
    ) -> Result<(), OrbitError>;
}
