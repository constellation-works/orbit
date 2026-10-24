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
use orbit_tools::ReservationOwnerContext;
use serde_json::Value;

use crate::application::contracts::PublishedDelivery;
use crate::runtime::task::locks::ReservationOutcome;

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
}
