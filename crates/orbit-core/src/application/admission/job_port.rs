//! Backlog admission's side of the job-owned port [ORB-17938].

use std::collections::BTreeSet;

use orbit_common::OrbitError;
use orbit_tools::ReservationOwnerContext;
use serde_json::Value;

use super::claim_holders::ClaimHolders;
use crate::OrbitRuntime;
use crate::application::contracts::PublishedDelivery;
use crate::application::job::JobAdmissionPort;
use crate::runtime::task::locks::ReservationOutcome;

impl JobAdmissionPort for OrbitRuntime {
    fn published_delivery_reservation(
        &self,
        input: &Value,
        denied: &ReservationOutcome,
        agent: Option<String>,
        model: Option<String>,
        owner: Option<ReservationOwnerContext>,
    ) -> Result<Option<(ReservationOutcome, PublishedDelivery)>, OrbitError> {
        self.deferred_footprint_reservation(input, denied, agent, model, owner)
    }

    fn execution_claims_holding(
        &self,
        task_ids: &BTreeSet<String>,
    ) -> (Vec<Value>, Option<String>) {
        let claim_holders = ClaimHolders::read(self);
        (
            claim_holders.json_for(task_ids.iter().map(String::as_str)),
            claim_holders.detail_for(task_ids.iter().map(String::as_str)),
        )
    }
}
