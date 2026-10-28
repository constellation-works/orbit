use orbit_common::OrbitError;
use orbit_store::contracts::{AdmissionLookup, AdmissionRequest};
use orbit_types::tool::ToolSessionContext;

use crate::OrbitRuntime;
use crate::application::distributed::ensure_distributed_mutation_available;

use super::model::TaskPullResponse;

impl OrbitRuntime {
    /// Serve one pull admission for a caller this session speaks for.
    ///
    /// Refusal order follows the spec: the distributed gate and destination
    /// authority first, then the trusted caller identity, then the store's
    /// shape/version/mode/policy ladder inside [`orbit_store::TaskCommitBoundary::admit_task`].
    /// A new request must carry the ship contract this owner resolves now — the
    /// one the probe reported — because the receipt freezes it; a replay keeps
    /// whatever its original request carried and is compared byte-for-byte.
    pub fn serve_task_pull(
        &self,
        session: &ToolSessionContext,
        request: &AdmissionRequest,
    ) -> Result<TaskPullResponse, OrbitError> {
        ensure_distributed_mutation_available("orbit.task.pull")?;
        self.ensure_distributed_owner_workspace()?;
        let identity =
            self.session_admission_identity(session, request.run_context.machine_name.as_deref())?;
        let boundary = self.admission_boundary()?;
        if matches!(
            boundary.lookup_admission(&identity, &request.request_id)?,
            AdmissionLookup::NotFound
        ) {
            let mut owner = self.owner_ship_contract_for(Some(&identity.location().machine_id));
            self.adopt_requested_review_crew(&mut owner, &request.ship);
            if request.ship != owner {
                return Err(OrbitError::InvalidInput(format!(
                    "ship_contract_mismatch: this owner now resolves mode '{}', base '{}', landing \
                     '{}', before-PR review {}, before-landing review {}, completion '{}' for this \
                     machine; re-read the probe before sending a new request",
                    owner.mode,
                    owner.base_branch,
                    owner.landing_branch,
                    crate::application::distributed::contract::on_off(owner.before_pr),
                    crate::application::distributed::contract::on_off(owner.before_landing),
                    owner.completion
                )));
            }
        }
        match self.admit_pull_request(&boundary, &identity, request)? {
            AdmissionLookup::Found {
                receipt,
                current_claim,
            } => Ok(TaskPullResponse {
                receipt: *receipt,
                claim_state: current_claim.map(|claim| claim.phase),
            }),
            AdmissionLookup::Expired => Err(OrbitError::InvalidInput("request_expired".into())),
            AdmissionLookup::NotFound => Err(OrbitError::Store(
                "admission committed no receipt for this request".into(),
            )),
        }
    }
}
