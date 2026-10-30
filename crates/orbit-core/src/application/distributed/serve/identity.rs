use orbit_common::OrbitError;
use orbit_store::TaskCommitBoundary;
use orbit_store::contracts::{
    AdmissionIdentity, AdmissionLookup, AdmissionReceipt, ExecutionClaim,
};
use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};
use orbit_types::tool::ToolSessionContext;

use crate::OrbitRuntime;
use crate::application::distributed::contract::{session_machine_id, trusted_identity};

use super::refusal;

impl OrbitRuntime {
    /// The receipt this owner recorded when it admitted `claim_id`'s claim.
    pub(crate) fn claim_receipt(&self, claim_id: &str) -> Result<AdmissionReceipt, OrbitError> {
        let claim = self.current_claim(claim_id)?;
        let AdmissionLookup::Found { receipt, .. } = self.admission_boundary()?.lookup_admission(
            &AdmissionIdentity::trusted_local(claim.executed_on.clone()),
            &claim.request_id,
        )?
        else {
            return Err(refusal::refused("original claim receipt unavailable"));
        };
        Ok(*receipt)
    }

    pub(crate) fn admission_boundary(&self) -> Result<TaskCommitBoundary, OrbitError> {
        TaskCommitBoundary::new(
            self.sqlite_store()?,
            TaskRegistryStore::open(&task_registry_path(&self.global_root()))?,
            self.workspace_id()?,
        )
    }

    /// The machine this session speaks for. Required: a claim is fenced on it,
    /// so a session the transport could not attribute gets no admission.
    pub(super) fn session_caller_machine(
        &self,
        session: &ToolSessionContext,
    ) -> Result<String, OrbitError> {
        session_machine_id(session).ok_or_else(|| {
            OrbitError::InvalidInput(
                "no trusted caller machine on this session; a follower reaches \
                 the owner through federated SSH, which names its machine"
                    .into(),
            )
        })
    }

    pub(super) fn session_admission_identity(
        &self,
        session: &ToolSessionContext,
        display_name: Option<&str>,
    ) -> Result<AdmissionIdentity, OrbitError> {
        let machine = self.session_caller_machine(session)?;
        Ok(trusted_identity(&machine, session, display_name))
    }

    /// The claim a follower names, read from the journal. Absence is the
    /// stale-attempt answer: a revoked or superseded claim is still listed, so
    /// an id that resolves to nothing was never this owner's.
    pub(super) fn current_claim(&self, claim_id: &str) -> Result<ExecutionClaim, OrbitError> {
        let claim_id = claim_id.trim();
        if claim_id.is_empty() {
            return Err(OrbitError::InvalidInput("`claim_id` is required".into()));
        }
        self.inspect_execution_claims()?
            .into_iter()
            .map(|inspection| inspection.claim)
            .find(|claim| claim.claim_id == claim_id)
            .ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "stale_claim: claim '{claim_id}' is not held by this owner workspace"
                ))
            })
    }
}
