//! Internal admission foundation. No tool or drain invokes this until claim
//! binding, fencing and settlement are integrated by the lifecycle layer.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::fs::path::workspace_relative_paths_overlap;
use orbit_common::fs::selector::canonical_selector_in_workspace;
use orbit_types::task::{TaskStatus, automatic_dispatch_cmp};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::TaskCommitBoundary;
use crate::contracts::*;
use crate::repository::task::v2::TaskV2Store;

const RECEIPT_KIND: &str = "distributed-admission-receipt-v1";
const CLAIM_KIND: &str = "distributed-execution-claim-v1";
pub const ADMISSION_RESERVATION_TTL_SECONDS: u32 = 14_400;

#[derive(Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum StoredReceipt {
    Full {
        receipt: Box<AdmissionReceipt>,
    },
    Tombstone {
        machine_id: String,
        request_id: String,
        input_digest: String,
    },
}

fn encode<T: Serialize>(value: &T) -> Result<String, OrbitError> {
    serde_json::to_string(value).map_err(|e| OrbitError::Store(e.to_string()))
}
fn decode<T: serde::de::DeserializeOwned>(value: &str) -> Result<T, OrbitError> {
    serde_json::from_str(value).map_err(|e| OrbitError::Store(e.to_string()))
}
fn digest<T: Serialize>(value: &T) -> Result<String, OrbitError> {
    Ok(format!("{:x}", Sha256::digest(encode(value)?.as_bytes())))
}
fn receipt_key(machine: &str, request: &str) -> Result<String, OrbitError> {
    digest(&(machine, request))
}
fn row<T: Serialize>(kind: &str, id: &str, value: &T) -> Result<TaskCoordinationRow, OrbitError> {
    Ok(TaskCoordinationRow {
        kind: kind.into(),
        row_id: id.into(),
        payload_json: encode(value)?,
    })
}
fn canonical_footprint(files: &[String], root: &Path) -> Result<Vec<String>, OrbitError> {
    if files.is_empty() {
        return Err(OrbitError::InvalidInput(
            "empty own context footprint".into(),
        ));
    }
    files
        .iter()
        .map(|f| {
            canonical_selector_in_workspace(f, root)
                .map_err(|e| OrbitError::InvalidInput(format!("invalid context selector {f}: {e}")))
        })
        .collect::<Result<BTreeSet<_>, _>>()
        .map(|files| files.into_iter().collect())
}
fn overlaps(left: &[String], right: &[String]) -> bool {
    left.iter()
        .any(|a| right.iter().any(|b| workspace_relative_paths_overlap(a, b)))
}

impl TaskCommitBoundary {
    pub(crate) fn guard_ordinary_footprint(
        &self,
        status: TaskStatus,
        files: &[String],
    ) -> Result<(), OrbitError> {
        if !matches!(status, TaskStatus::InProgress | TaskStatus::Review)
            || self.execution_claims()?.is_empty()
        {
            return Ok(());
        }
        let checkout = self
            .registry
            .find_workspace_checkout(&self.workspace_id)?
            .ok_or_else(|| OrbitError::Store("claimed workspace checkout is unavailable".into()))?;
        if files.is_empty() {
            return Ok(());
        } // legacy no-context chore semantics
        let canonical = canonical_footprint(files, &checkout.repo_root)?;
        if !self.frozen_claim_conflicts(&canonical)?.is_empty() {
            return Err(OrbitError::InvalidInput(
                "task footprint overlaps an execution claim".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn refuse_unscoped_claim_write(&self, task_id: &str) -> Result<(), OrbitError> {
        if self
            .execution_claims()?
            .iter()
            .any(|c| c.task_id == task_id && c.phase.protects_footprint())
        {
            return Err(OrbitError::InvalidInput(
                "active execution claim requires a claim-scoped mutation".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn frozen_claim_conflicts(
        &self,
        files: &[String],
    ) -> Result<Vec<TaskLockConflict>, OrbitError> {
        Ok(self
            .execution_claims()?
            .iter()
            .filter(|claim| claim.phase.protects_footprint())
            .flat_map(|claim| {
                files
                    .iter()
                    .filter(|file| overlaps(std::slice::from_ref(*file), &claim.footprint))
                    .map(|file| TaskLockConflict {
                        file: file.clone(),
                        held_by: TaskLockHolder::Task,
                        held_by_id: claim.task_id.clone(),
                    })
                    .collect::<Vec<_>>()
            })
            .collect())
    }

    fn receipt_row(
        &self,
        machine: &str,
        request: &str,
    ) -> Result<Option<TaskCoordinationRow>, OrbitError> {
        let key = receipt_key(machine, request)?;
        Ok(self
            .coordination_rows(RECEIPT_KIND)?
            .into_iter()
            .find(|row| row.row_id == key))
    }

    pub fn execution_claims(&self) -> Result<Vec<ExecutionClaim>, OrbitError> {
        self.enter_ordinary(|| {
            self.coordination_rows(CLAIM_KIND)?
                .iter()
                .map(|row| decode(&row.payload_json))
                .collect()
        })
    }

    /// Reconciliation does not reapply old binary or ship compatibility checks.
    /// The caller must authenticate and authorize the identity on every call.
    pub fn lookup_admission(
        &self,
        identity: &AdmissionIdentity,
        request_id: &str,
    ) -> Result<AdmissionLookup, OrbitError> {
        self.with_admission(|| {
            let Some(row) = self.receipt_row(&identity.location().machine_id, request_id)? else {
                return Ok(AdmissionLookup::NotFound);
            };
            match decode::<StoredReceipt>(&row.payload_json)? {
                StoredReceipt::Tombstone { .. } => Ok(AdmissionLookup::Expired),
                StoredReceipt::Full { receipt } => {
                    let current_claim = match &receipt.claim {
                        None => None,
                        Some(original) => self
                            .execution_claims()?
                            .into_iter()
                            .find(|claim| claim.claim_id == original.claim_id),
                    };
                    Ok(AdmissionLookup::Found {
                        receipt,
                        current_claim: current_claim.map(Box::new),
                    })
                }
            }
        })
    }

    /// One internal owner admission. Identity and resolved ship authority are
    /// trusted arguments supplied after authorization, never payload identity.
    /// No local run, worktree, branch or public pull endpoint is created here.
    pub fn admit_task(
        &self,
        identity: &AdmissionIdentity,
        request: &AdmissionRequest,
        owner_version: &str,
        repo_root: &Path,
        orbit_dir: &Path,
    ) -> Result<AdmissionLookup, OrbitError> {
        validate_request(identity, request, owner_version)?;
        self.with_admission(|| self.admit_locked(identity, request, repo_root, orbit_dir))
    }

    fn admit_locked(
        &self,
        identity: &AdmissionIdentity,
        request: &AdmissionRequest,
        repo_root: &Path,
        orbit_dir: &Path,
    ) -> Result<AdmissionLookup, OrbitError> {
        if let Some(row) = self.receipt_row(&identity.location().machine_id, &request.request_id)? {
            let previous = decode::<StoredReceipt>(&row.payload_json)?;
            let same = match previous {
                StoredReceipt::Full { receipt } => receipt.request == *request,
                StoredReceipt::Tombstone { input_digest, .. } => input_digest == digest(request)?,
            };
            if !same {
                return Err(OrbitError::InvalidInput("request_mismatch".into()));
            }
            return self.lookup_admission(identity, &request.request_id);
        }
        let translator = TaskV2Store::new(self.registry.clone(), self.workspace_id.clone());
        let mut tasks = self
            .bundle_store
            .list_bundles()?
            .into_iter()
            .map(|bundle| translator.task_from_bundle(bundle))
            .collect::<Result<Vec<_>, _>>()?;
        tasks.sort_by(automatic_dispatch_cmp);
        let mut statuses: BTreeMap<_, _> = tasks.iter().map(|t| (t.id.clone(), t.status)).collect();
        for dependency in tasks
            .iter()
            .flat_map(|t| t.dependencies())
            .collect::<BTreeSet<_>>()
        {
            if statuses.contains_key(&dependency) {
                continue;
            }
            let Some(binding) = self.registry.find_task_binding(&dependency)? else {
                continue;
            };
            // The host admission lock excludes every partition's ordinary
            // writers, so a cross-workspace dependency cannot move here.
            let owner = TaskCommitBoundary {
                store: self.store.clone(),
                registry: self.registry.clone(),
                bundle_store: crate::repository::task::v2_bundle::TaskBundleStoreV2::new(
                    self.registry.clone(),
                    binding.partition_id.clone(),
                ),
                workspace_id: binding.partition_id.clone(),
                partition_dir: self
                    .registry
                    .workspace_partition_dir(&binding.partition_id)?,
            };
            owner.verify_journal_binding()?;
            owner.recover_if_pending()?;
            match owner.bundle_store.read_bundle_lightweight(&dependency) {
                Ok(bundle) => {
                    statuses.insert(dependency, bundle.envelope.status);
                }
                Err(OrbitError::NotFound { .. }) => {}
                Err(error) => return Err(error),
            }
        }
        let claims = self.execution_claims()?;
        let reservations = self.store.inspect_active_task_reservations(
            &orbit_dir.to_string_lossy(),
            Some(&self.workspace_id),
        )?;
        let mut receipt = AdmissionReceipt {
            schema_version: 1,
            request: request.clone(),
            machine_id: identity.location().machine_id.clone(),
            claim: None,
            task: None,
            invalid_candidates: Vec::new(),
            deferred_conflicts: Vec::new(),
            queue_depth: tasks
                .iter()
                .filter(|task| {
                    task.status == TaskStatus::Backlog
                        && task
                            .dependencies()
                            .iter()
                            .all(|id| statuses.get(id) == Some(&TaskStatus::Done))
                })
                .count(),
        };
        let key = receipt_key(&receipt.machine_id, &request.request_id)?;
        for task in tasks
            .iter()
            .filter(|task| task.status == TaskStatus::Backlog)
        {
            if task
                .dependencies()
                .iter()
                .any(|id| statuses.get(id) != Some(&TaskStatus::Done))
            {
                receipt.invalid_candidates.push(AdmissionDiagnostic {
                    task_id: task.id.clone(),
                    reason: "dependency is missing or not done".into(),
                });
                continue;
            }
            let footprint = match canonical_footprint(&task.context_files, repo_root) {
                Ok(files) => files,
                Err(error) => {
                    receipt.invalid_candidates.push(AdmissionDiagnostic {
                        task_id: task.id.clone(),
                        reason: error.to_string(),
                    });
                    continue;
                }
            };
            let claim_conflict = claims.iter().find(|claim| {
                claim.phase.protects_footprint()
                    && (claim.task_id == task.id || overlaps(&footprint, &claim.footprint))
            });
            let status_conflict = tasks
                .iter()
                .filter(|t| matches!(t.status, TaskStatus::InProgress | TaskStatus::Review))
                .find(|held| {
                    held.context_files.iter().any(|file| {
                        canonical_selector_in_workspace(file, repo_root)
                            .is_ok_and(|file| overlaps(&footprint, &[file]))
                    })
                });
            let reservation_conflict = reservations.iter().find(|r| overlaps(&footprint, &r.files));
            let blocker = claim_conflict
                .map(|c| c.task_id.as_str())
                .or_else(|| status_conflict.map(|t| t.id.as_str()))
                .or_else(|| reservation_conflict.map(|r| r.reservation_id.as_str()));
            if let Some(blocker) = blocker {
                receipt.deferred_conflicts.push(AdmissionDiagnostic {
                    task_id: task.id.clone(),
                    reason: format!("protected footprint held by {blocker}"),
                });
                continue;
            }
            let claim_id = format!("claim-{}", digest(&(&self.workspace_id, &key))?);
            let params = TaskCoordinationCommitParams {
                task_id: task.id.clone(),
                actor: receipt.machine_id.clone(),
                expected_status: vec![TaskStatus::Backlog],
                status: Some(TaskStatus::InProgress),
                status_event: Some("pulled_by".into()),
                status_note: Some(encode(
                    &serde_json::json!({"machine_id":receipt.machine_id,
                    "run_context":request.run_context,"claim_id":claim_id,"request_id":request.request_id}),
                )?),
                reservation: Some(TaskReservationReserveParams {
                    workspace_orbit_dir: orbit_dir.to_string_lossy().into_owned(),
                    workspace_id: Some(self.workspace_id.clone()),
                    task_ids: vec![task.id.clone()],
                    requested_files: footprint.clone(),
                    actor: receipt.machine_id.clone(),
                    ttl_seconds: ADMISSION_RESERVATION_TTL_SECONDS,
                    owner_run_id: None,
                    owner_metadata_json: Some(encode(&serde_json::json!({"claim_id":claim_id}))?),
                }),
                rows: vec![
                    row(RECEIPT_KIND, &key, &())?,
                    row(CLAIM_KIND, &claim_id, &())?,
                ],
                ..Default::default()
            };
            let committed = self.commit_locked_with_rows(&params, &mut |reservation| {
                let reserved = reservation
                    .ok_or_else(|| OrbitError::Store("admission reservation missing".into()))?;
                let claim = ExecutionClaim {
                    claim_id: claim_id.clone(),
                    task_id: task.id.clone(),
                    request_id: request.request_id.clone(),
                    executed_on: identity.location().clone(),
                    run_context: request.run_context.clone(),
                    footprint: footprint.clone(),
                    phase: ExecutionClaimPhase::Claimed,
                    reservation_id: reserved
                        .reservation_id
                        .clone()
                        .ok_or_else(|| OrbitError::Store("reservation id missing".into()))?,
                    reservation_expires_at: reserved
                        .expires_at
                        .clone()
                        .ok_or_else(|| OrbitError::Store("reservation expiry missing".into()))?,
                };
                let mut admitted = receipt.clone();
                admitted.claim = Some(claim.clone());
                admitted.task = Some(AdmissionTaskSummary {
                    id: task.id.clone(),
                    title: task.title.clone(),
                    complexity: task.complexity,
                    crew: task.crew.clone(),
                    context_files: footprint.clone(),
                });
                admitted.queue_depth = admitted.queue_depth.saturating_sub(1);
                Ok(vec![
                    row(
                        RECEIPT_KIND,
                        &key,
                        &StoredReceipt::Full {
                            receipt: Box::new(admitted),
                        },
                    )?,
                    row(CLAIM_KIND, &claim_id, &claim)?,
                ])
            })?;
            if !matches!(committed, TaskCoordinationCommitOutcome::Committed(_)) {
                return Err(OrbitError::Store(
                    "admission changed inside the serialization boundary".into(),
                ));
            }
            return self.lookup_admission(identity, &request.request_id);
        }
        self.store.insert_task_coordination_row(
            &self.workspace_id,
            &row(
                RECEIPT_KIND,
                &key,
                &StoredReceipt::Full {
                    receipt: Box::new(receipt),
                },
            )?,
        )?;
        self.lookup_admission(identity, &request.request_id)
    }

    /// Never compact an unsettled claim. Tombstones have no deletion API in v1.
    pub fn compact_admission(
        &self,
        identity: &AdmissionIdentity,
        request_id: &str,
    ) -> Result<bool, OrbitError> {
        self.with_admission(|| {
            let Some(existing) = self.receipt_row(&identity.location().machine_id, request_id)?
            else {
                return Ok(false);
            };
            let StoredReceipt::Full { receipt } = decode(&existing.payload_json)? else {
                return Ok(false);
            };
            if let Some(claim) = &receipt.claim {
                let current = self
                    .execution_claims()?
                    .into_iter()
                    .find(|c| c.claim_id == claim.claim_id)
                    .ok_or_else(|| OrbitError::Store("admission claim is missing".into()))?;
                if current.phase.is_unsettled() {
                    return Ok(false);
                }
            }
            self.store.replace_task_coordination_payload(
                &self.workspace_id,
                &existing,
                &encode(&StoredReceipt::Tombstone {
                    machine_id: receipt.machine_id.clone(),
                    request_id: request_id.into(),
                    input_digest: digest(&receipt.request)?,
                })?,
            )
        })
    }

    pub fn admission_storage_usage(&self) -> Result<AdmissionStorageUsage, OrbitError> {
        self.enter_ordinary(|| {
            let mut usage = AdmissionStorageUsage::default();
            for row in self.coordination_rows(RECEIPT_KIND)? {
                match decode::<StoredReceipt>(&row.payload_json)? {
                    StoredReceipt::Full { .. } => {
                        usage.receipts += 1;
                        usage.receipt_bytes += row.payload_json.len() as u64;
                    }
                    StoredReceipt::Tombstone { .. } => {
                        usage.tombstones += 1;
                        usage.tombstone_bytes += row.payload_json.len() as u64;
                    }
                }
            }
            Ok(usage)
        })
    }
}

/// The ordered pre-admission ladder, shared by admission and the read-only
/// preflight probe.
///
/// Selector resolution, session capability, and trusted invocation context are
/// decided by the calling surface before this function is reached; identity
/// here is already trusted. Order is the spec's: input shape, then
/// version/schema, then ship mode, then review policy. Evaluating it returns a
/// verdict and writes nothing, so a probe and an admission cannot disagree
/// about what would be refused.
pub fn admission_refusal(
    identity: &AdmissionIdentity,
    request: &AdmissionRequest,
    owner_version: &str,
) -> Option<AdmissionRefusal> {
    if [
        &identity.location().machine_id,
        &request.request_id,
        &request.caller_version,
        &request.run_context.run_id,
        &request.run_context.job_name,
    ]
    .iter()
    .any(|v| v.trim().is_empty())
    {
        return Some(AdmissionRefusal::InvalidInput);
    }
    if request.caller_schema == 0
        || request.ship.base_branch.trim().is_empty()
        || request.ship.landing_branch.trim().is_empty()
        || !matches!(request.ship.completion.as_str(), "review" | "done")
        || (request.ship.completion == "done"
            && request
                .ship
                .authorization_reference
                .as_deref()
                .is_none_or(|r| r.trim().is_empty()))
    {
        return Some(AdmissionRefusal::InvalidInput);
    }
    if request.caller_version != owner_version
        || request.caller_schema != DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA
    {
        return Some(AdmissionRefusal::VersionMismatch);
    }
    if !matches!(request.ship.mode.as_str(), "pr" | "local")
        || (identity.is_remote() && request.ship.mode == "local")
    {
        return Some(AdmissionRefusal::ShipModeUnsupported);
    }
    // Both endpoints must say `none`; every other policy is rejected by name
    // rather than downgraded.
    if request.caller_review_policy != "none" || request.ship.review_policy != "none" {
        return Some(AdmissionRefusal::ReviewPolicyUnsupported);
    }
    None
}

fn validate_request(
    identity: &AdmissionIdentity,
    request: &AdmissionRequest,
    version: &str,
) -> Result<(), OrbitError> {
    match admission_refusal(identity, request, version) {
        Some(refusal) => Err(OrbitError::InvalidInput(refusal.as_str().into())),
        None => Ok(()),
    }
}
