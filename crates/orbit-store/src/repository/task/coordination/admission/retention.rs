//! Explicit admission receipt maintenance; tombstone identities never expire.

use super::super::Section;
use super::{CLAIM_KIND, RECEIPT_KIND, StoredReceipt, TaskCommitBoundary, decode, digest, row};
use crate::contracts::{AdmissionReceiptMaintenance, AdmissionReceiptStorage, ExecutionClaim};
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

    /// Compact idle and settled receipts atomically under the same exclusive
    /// boundary as admission and claim mutations. No identity is deleted.
    pub fn compact_admission_receipts(&self) -> Result<AdmissionReceiptMaintenance, OrbitError> {
        self.with_admission(|| {
            let mut compacted = Vec::new();
            for existing in self
                .store
                .task_coordination_rows(&self.workspace_id, RECEIPT_KIND)?
            {
                let StoredReceipt::Full { receipt } = decode(&existing.payload_json)? else {
                    continue;
                };
                if let Some(original) = &receipt.claim {
                    let current = self.store.task_coordination_row(
                        &self.workspace_id,
                        CLAIM_KIND,
                        &original.claim_id,
                    )?;
                    // Missing is not proof of settlement. Fail closed on a
                    // malformed row too, before publishing any compaction.
                    let Some(current) = current else { continue };
                    let claim: ExecutionClaim = decode(&current.payload_json)?;
                    if claim.phase.is_unsettled() {
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
