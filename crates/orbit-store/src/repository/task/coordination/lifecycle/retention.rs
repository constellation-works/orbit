//! Retention for settled claims' mutation receipts.
//!
//! A receipt replays a committed claim mutation's outcome. Once its claim is
//! settled every mutation of it is refused `stale_claim` except an operator
//! recovery of a failed claim, so the retained claim row, not the receipt,
//! is what keeps an old mutation ID from acting again. Claim and lifecycle
//! rows are never deleted: they are that retired namespace, and release
//! budgets, kept candidates and the one-leaf-per-attempt check read them.

use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;

use super::super::TaskCommitBoundary;
use super::codec::{CLAIM, FRICTION, RECEIPT, STATE};
use super::decode;
use crate::contracts::{ClaimInspection, ExecutionClaim};

/// Days a settled claim's mutation receipts outlive its last mutation.
const SETTLED_RECEIPT_RETENTION_DAYS: i64 = 7;

impl TaskCommitBoundary {
    /// Prune after a mutation that left its claim settled. Failure is logged
    /// and never undoes or fails the committed mutation; the next settlement
    /// retries.
    pub(super) fn prune_settled_receipts_after_settlement(&self) {
        let cutoff = Utc::now() - Duration::days(SETTLED_RECEIPT_RETENTION_DAYS);
        if let Err(error) = self.prune_settled_claim_receipts(cutoff) {
            orbit_common::tracing::warn!(
                target: "orbit.store.claim_retention",
                workspace_id = %self.workspace_id,
                %error,
                "pruning settled claim mutation receipts failed; the next settlement retries",
            );
        }
    }

    /// Delete the mutation receipts, and the friction replay rows keyed by
    /// them, of every claim that is settled and was last mutated before
    /// `settled_before`. Returns how many receipts went.
    fn prune_settled_claim_receipts(
        &self,
        settled_before: DateTime<Utc>,
    ) -> Result<usize, OrbitError> {
        self.enter_ordinary(|| {
            let mut settled = Vec::new();
            for row in self
                .store
                .task_coordination_rows(&self.workspace_id, CLAIM)?
            {
                let claim: ExecutionClaim = decode(&row.payload_json)?;
                if claim.phase.is_unsettled() {
                    continue;
                }
                let Some(state) =
                    self.store
                        .task_coordination_row(&self.workspace_id, STATE, &claim.claim_id)?
                else {
                    continue;
                };
                let state: ClaimInspection = decode(&state.payload_json)?;
                let updated = DateTime::parse_from_rfc3339(&state.updated_at)
                    .map_err(|e| OrbitError::Store(e.to_string()))?;
                if updated < settled_before {
                    settled.push(claim.claim_id);
                }
            }
            if settled.is_empty() {
                return Ok(0);
            }
            self.store.delete_claim_mutation_receipts(
                &self.workspace_id,
                RECEIPT,
                FRICTION,
                &settled,
                &settled_before.to_rfc3339(),
            )
        })
    }
}
