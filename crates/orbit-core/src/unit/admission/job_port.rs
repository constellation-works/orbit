//! Backlog admission's side of the job-owned port [ORB-17938].

use std::collections::BTreeSet;

use orbit_common::OrbitError;
use orbit_store::contract::TaskLockConflict;
use orbit_tools::ReservationOwnerContext;
use orbit_types::task::Task;
use serde_json::Value;

use super::backlog_exclusion;
use super::claim_holder::ClaimHolders;
use crate::OrbitRuntime;
use crate::application::contracts::PublishedDelivery;
use crate::application::job::JobAdmissionPort;
use crate::runtime::stored::locks::ReservationOutcome;

/// Adapter binding the job admission port to the admission unit.
pub(crate) struct JobPort<'a>(pub(crate) &'a OrbitRuntime);

impl JobAdmissionPort for JobPort<'_> {
    fn preview_named_ship(&self, input: &Value) -> Result<Value, OrbitError> {
        backlog_exclusion::preview_named_backlog_tasks(self.0, input)
            .map_err(|error| OrbitError::PolicyDenied(error.to_string()))
    }

    fn published_delivery_reservation(
        &self,
        input: &Value,
        denied: &ReservationOutcome,
        agent: Option<String>,
        model: Option<String>,
        owner: Option<ReservationOwnerContext>,
    ) -> Result<Option<(ReservationOutcome, PublishedDelivery)>, OrbitError> {
        super::published_delivery::deferred_footprint_reservation(
            self.0, input, denied, agent, model, owner,
        )
    }

    fn execution_claims_holding(
        &self,
        task_ids: &BTreeSet<String>,
    ) -> (Vec<Value>, Option<String>) {
        let claim_holders = ClaimHolders::read(self.0);
        (
            claim_holders.json_for(task_ids.iter().map(String::as_str)),
            claim_holders.detail_for(task_ids.iter().map(String::as_str)),
        )
    }

    fn operator_overridden_gates(&self, task: &Task, schedule_held: bool) -> Vec<String> {
        backlog_exclusion::operator_overridden_gates(task, schedule_held)
    }

    fn lock_conflict_gate(&self, conflicts: &[TaskLockConflict]) -> String {
        backlog_exclusion::lock_conflict_gate(self.0, conflicts)
    }

    fn record_readiness_override(
        &self,
        task_id: &str,
        gates: &[String],
        actor: &str,
    ) -> Result<(), OrbitError> {
        backlog_exclusion::record_readiness_override(self.0, task_id, gates, actor)
    }
}
