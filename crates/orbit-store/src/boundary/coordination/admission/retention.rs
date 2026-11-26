//! Explicit admission receipt maintenance; tombstone identities never expire.

use std::collections::BTreeSet;

use super::super::Section;
use super::{RECEIPT_KIND, StoredReceipt, TaskCommitBoundary, decode, digest, row};
use crate::contracts::{AdmissionReceiptMaintenance, AdmissionReceiptStorage, ExecutionClaimPhase};
use orbit_common::OrbitError;

const CLAIM_MUTATION_RECEIPT_KIND: &str = "distributed-claim-mutation-v1";

impl TaskCommitBoundary {
    /// Read logical receipt counts and bytes without recovering interrupted
    /// commits. Like claim inspection, this diagnostic never repairs state.
    pub fn inspect_admission_receipt_storage(&self) -> Result<AdmissionReceiptStorage, OrbitError> {
        let section = Section::here("receipt inspection");
        self.shared(&self.host_lock_target(), section, || {
            self.shared(&self.lock_target(), section, || {
                if self.interrupted_commit()? {
                    return Err(OrbitError::Store(
                        "receipt inspection unavailable until pending commit is recovered".into(),
                    ));
                }
                self.admission_receipt_storage_locked()
            })
        })
    }

    /// Compact idle and settled receipts that no longer authorize a kept PR,
    /// atomically with admission and claim mutations. No identity is deleted.
    pub fn compact_admission_receipts(&self) -> Result<AdmissionReceiptMaintenance, OrbitError> {
        self.with_admission(|| {
            // Read lifecycle state under the same boundary as receipt updates.
            // Failed/revoked claims still need their original ship contract to
            // authorize external settlement of a PR they kept, even without an
            // accepted handoff. Malformed state fails before any compaction.
            let compactable_claims: BTreeSet<_> = self
                .claim_states_locked()?
                .into_iter()
                .filter(|state| {
                    !state.claim.phase.is_unsettled()
                        && !(matches!(
                            state.claim.phase,
                            ExecutionClaimPhase::Failed | ExecutionClaimPhase::Revoked
                        ) && state
                            .preserved_candidate
                            .as_ref()
                            .is_some_and(|kept| kept.candidate.pull_request.is_some()))
                })
                .map(|state| state.claim.claim_id)
                .collect();
            let mut compacted = Vec::new();
            for existing in self
                .store
                .task_coordination_rows(&self.workspace_id, RECEIPT_KIND)?
            {
                let StoredReceipt::Full { receipt } = decode(&existing.payload_json)? else {
                    continue;
                };
                if let Some(original) = &receipt.claim {
                    // Missing is not proof of settlement.
                    if !compactable_claims.contains(&original.claim_id) {
                        continue;
                    }
                }
                let tombstone = StoredReceipt::Tombstone {
                    machine_id: receipt.machine_id,
                    request_id: receipt.request.request_id.clone(),
                    input_digest: digest(&receipt.request)?,
                };
                compacted.push(row(RECEIPT_KIND, &existing.row_id, &tombstone)?);
            }
            self.store
                .upsert_sql_task_coordination_rows(&self.workspace_id, &compacted)?;
            Ok(AdmissionReceiptMaintenance {
                compacted_receipts: compacted.len() as u64,
                storage: self.admission_receipt_storage_locked()?,
            })
        })
    }

    fn admission_receipt_storage_locked(&self) -> Result<AdmissionReceiptStorage, OrbitError> {
        let mut usage = AdmissionReceiptStorage::default();
        for row in self
            .store
            .task_coordination_rows(&self.workspace_id, RECEIPT_KIND)?
        {
            let category = match decode::<StoredReceipt>(&row.payload_json)? {
                StoredReceipt::Full { .. } => &mut usage.full_receipts,
                StoredReceipt::Tombstone { .. } => &mut usage.tombstones,
            };
            category.count += 1;
            category.logical_payload_bytes += row.payload_json.len() as u64;
        }
        for row in self
            .store
            .task_coordination_rows(&self.workspace_id, CLAIM_MUTATION_RECEIPT_KIND)?
        {
            usage.claim_mutation_receipts.count += 1;
            usage.claim_mutation_receipts.logical_payload_bytes += row.payload_json.len() as u64;
        }
        Ok(usage)
    }
}
