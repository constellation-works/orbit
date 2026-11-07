//! Resolve an accepted handoff to its owning execution claim.

use orbit_common::{ClaimRefusalKind, OrbitError};
use orbit_store::contracts::ClaimInspection;
use orbit_types::workflow::handoff::AcceptedHandoff;

use crate::OrbitRuntime;

impl OrbitRuntime {
    /// Resolve a handoff to the claim that owns it. Claims are few and the
    /// handoff identity is a digest of the accepted record, so this is an exact
    /// match rather than a search over task state.
    pub(crate) fn landing_claim(
        &self,
        handoff_id: &str,
    ) -> Result<(ClaimInspection, AcceptedHandoff), OrbitError> {
        for claim in self.stores().tasks().resolve_execution_claims()? {
            let Some(accepted) = self
                .stores()
                .tasks()
                .find_accepted_handoff(&claim.claim.claim_id)?
            else {
                continue;
            };
            if accepted.handoff_id == handoff_id {
                return Ok((claim, accepted));
            }
        }
        Err(OrbitError::ClaimRefused {
            kind: ClaimRefusalKind::NotCurrent,
            message: format!("no accepted handoff '{handoff_id}' is current on this owner"),
        })
    }
}
