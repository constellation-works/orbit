//! Receipt and execution-claim reads, and the admission lookup built on them.

use orbit_common::OrbitError;

use super::super::TaskCommitBoundary;
use super::{CLAIM_KIND, RECEIPT_KIND, StoredReceipt, decode, receipt_key};
use crate::contracts::*;

impl TaskCommitBoundary {
    pub(super) fn receipt_row(
        &self,
        machine: &str,
        request: &str,
    ) -> Result<Option<TaskCoordinationRow>, OrbitError> {
        let key = receipt_key(machine, request)?;
        self.coordination_row(RECEIPT_KIND, &key)
    }

    pub fn execution_claims(&self) -> Result<Vec<ExecutionClaim>, OrbitError> {
        self.enter_ordinary(|| {
            self.coordination_rows(CLAIM_KIND)?
                .iter()
                .map(|row| decode(&row.payload_json))
                .collect()
        })
    }

    /// One claim by its indexed identity, after settling any pending commit.
    fn execution_claim(&self, claim_id: &str) -> Result<Option<ExecutionClaim>, OrbitError> {
        self.coordination_row(CLAIM_KIND, claim_id)?
            .map(|row| decode(&row.payload_json))
            .transpose()
    }

    /// Narrow in SQL before decoding; keep the exact task and phase checks
    /// because a payload may mention the task in another field as well.
    pub(crate) fn protecting_claim_for_task(
        &self,
        task_id: &str,
    ) -> Result<Option<ExecutionClaim>, OrbitError> {
        for row in self.coordination_rows_mentioning(CLAIM_KIND, task_id)? {
            let claim: ExecutionClaim = decode(&row.payload_json)?;
            if claim.task_id == task_id && claim.phase.protects_footprint() {
                return Ok(Some(claim));
            }
        }
        Ok(None)
    }

    /// Reconciliation does not reapply old binary or ship compatibility checks.
    /// The caller must authenticate and authorize the identity on every call.
    ///
    /// A pure read, so it shares the boundary with ordinary participants: a
    /// receipt and its claim are published together by an admission, which
    /// excludes this read until both have landed.
    pub fn lookup_admission(
        &self,
        identity: &AdmissionIdentity,
        request_id: &str,
    ) -> Result<AdmissionLookup, OrbitError> {
        self.enter_ordinary(|| {
            let Some(row) = self.receipt_row(&identity.location().machine_id, request_id)? else {
                return Ok(AdmissionLookup::NotFound);
            };
            match decode::<StoredReceipt>(&row.payload_json)? {
                StoredReceipt::Tombstone { .. } => Ok(AdmissionLookup::Expired),
                StoredReceipt::Full { receipt } => {
                    let current_claim = match &receipt.claim {
                        None => None,
                        Some(original) => self.execution_claim(&original.claim_id)?,
                    };
                    Ok(AdmissionLookup::Found {
                        receipt,
                        current_claim: current_claim.map(Box::new),
                    })
                }
            }
        })
    }
}
