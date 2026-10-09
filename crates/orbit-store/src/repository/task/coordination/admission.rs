//! Owner admission foundation. Reached only through `orbit-core`'s trusted
//! owner seams — the `orbit.task.pull` tool and the owner-local drain adapter —
//! which supply the caller identity from the session, never from the request.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::fs::path::workspace_relative_paths_overlap;
use orbit_common::fs::selector::canonical_selector_in_workspace;
use orbit_common::security::release::sha256_hex;
use orbit_types::task::{Task, TaskComment, TaskHistoryEntry, TaskStatus};
use serde::{Deserialize, Serialize};

use super::TaskCommitBoundary;
use super::lifecycle::fresh_offer_history;
use super::selection::{AdmissionSnapshot, Screen};
use crate::contracts::*;
use crate::repository::task::v2::{TaskV2Store, task_history_from_events};

/// The in-section re-check of assessment-scoped holds a caller gives
/// [`TaskCommitBoundary::admit_task`]: the candidate with the comments and
/// history read with it, to the hold's diagnostic, or `None` when it is free.
pub type ValidationHold<'a> =
    dyn Fn(&Task, &[TaskComment], &[TaskHistoryEntry]) -> Result<Option<String>, OrbitError> + 'a;

pub(super) const RECEIPT_KIND: &str = "distributed-admission-receipt-v1";
const CLAIM_KIND: &str = "distributed-execution-claim-v1";
pub const ADMISSION_RESERVATION_TTL_SECONDS: u32 = 14_400;

#[derive(Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(super) enum StoredReceipt {
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
pub(super) fn digest<T: Serialize>(value: &T) -> Result<String, OrbitError> {
    Ok(sha256_hex(encode(value)?.as_bytes()))
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
pub(super) fn canonical_footprint(
    files: &[String],
    root: &Path,
) -> Result<Vec<String>, OrbitError> {
    files
        .iter()
        .map(|f| {
            canonical_selector_in_workspace(f, root)
                .map_err(|e| OrbitError::InvalidInput(format!("invalid context selector {f}: {e}")))
        })
        .collect::<Result<BTreeSet<_>, _>>()
        .map(|files| files.into_iter().collect())
}
/// Why `task`'s `os:` tags exclude the requesting executor, or `None` when
/// they admit it.
pub(super) fn os_unavailable(
    task: &orbit_types::task::Task,
    request: &AdmissionRequest,
) -> Option<String> {
    let wait = orbit_types::task::TaskOsRequirement::from_tags(&task.tags)
        .unsatisfied_reason(request.os)?;
    Some(format!(
        "{wait}; the executor runs {}",
        request
            .os
            .map_or("an undeclared OS", orbit_types::task::HostOs::as_str)
    ))
}
pub(super) fn overlaps(left: &[String], right: &[String]) -> bool {
    left.iter()
        .any(|a| right.iter().any(|b| workspace_relative_paths_overlap(a, b)))
}
impl TaskCommitBoundary {
    pub(crate) fn guard_ordinary_footprint(
        &self,
        status: TaskStatus,
        files: &[String],
    ) -> Result<(), OrbitError> {
        if status != TaskStatus::InProgress {
            return Ok(());
        }
        let claims = self.execution_claims()?;
        if claims.is_empty() {
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
        if let Some(conflicting_claim) = claims.iter().find(|claim| {
            claim.phase.protects_footprint() && overlaps(&canonical, &claim.footprint)
        }) {
            return Err(OrbitError::InvalidInput(format!(
                "task footprint overlaps an execution claim (task {}, run {})",
                conflicting_claim.task_id, conflicting_claim.run_context.run_id
            )));
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
    ///
    /// Candidates are selected from the generated index before the exclusive
    /// section, which stalls every task write on the host [ORB-14724]. The
    /// section re-reads only what one decision rests on: the candidate, its
    /// dependencies, the in-flight tasks whose footprints can conflict, claims
    /// and reservations. A candidate that changed after selection is judged on
    /// what the section reads, so a stale selection can defer it but never
    /// admit it.
    ///
    /// `admission_holds` maps each task held by a live owner-local delivery,
    /// a successful pilot preparation or a current pilot finding to its
    /// trusted diagnostic. It is deferred even while it is still `backlog`: a
    /// local drain's gate waiting for context locks has neither moved the task
    /// nor reserved its footprint yet, so status and reservations alone would
    /// hand it out a second time [ORB-13918].
    ///
    /// `validation_hold` re-checks assessment-scoped holds inside the section
    /// for each candidate about to be committed, given the comments and
    /// history of the bundle read there; it must not read the task store
    /// itself. A pilot apply that lands after the caller computed
    /// `admission_holds` therefore cannot leave a newly held task claimed from
    /// an older snapshot.
    ///
    /// `held` maps each `backlog` task the owner is withholding for a red
    /// base to why [ORB-14258]. Its last delivery failed a required command
    /// the base fails the same way; it is deferred until the held command
    /// passes on a new base tip, as it would be on the owner's own drain.
    #[allow(clippy::too_many_arguments)]
    pub fn admit_task(
        &self,
        identity: &AdmissionIdentity,
        request: &AdmissionRequest,
        owner_version: &str,
        repo_root: &Path,
        orbit_dir: &Path,
        admission_holds: &BTreeMap<String, String>,
        held: &BTreeMap<String, String>,
        validation_hold: &ValidationHold<'_>,
    ) -> Result<AdmissionLookup, OrbitError> {
        validate_request(identity, request, owner_version)?;
        let snapshot = self.admission_snapshot()?;
        #[cfg(test)]
        super::selection::after_selection::run();
        self.with_admission(|| {
            self.admit_locked(
                identity,
                request,
                repo_root,
                orbit_dir,
                &snapshot,
                admission_holds,
                held,
                validation_hold,
            )
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn admit_locked(
        &self,
        identity: &AdmissionIdentity,
        request: &AdmissionRequest,
        repo_root: &Path,
        orbit_dir: &Path,
        snapshot: &AdmissionSnapshot,
        admission_holds: &BTreeMap<String, String>,
        held: &BTreeMap<String, String>,
        validation_hold: &ValidationHold<'_>,
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
        let in_flight = self.in_flight_locked(snapshot, repo_root)?;
        let claims = self.execution_claims()?;
        let drain_released =
            self.drain_releases(&identity.location().machine_id, &request.run_context.run_id)?;
        let reservations = self.store.inspect_active_task_reservations(
            &orbit_dir.to_string_lossy(),
            Some(&self.workspace_id),
        )?;
        let screen = Screen {
            identity,
            request,
            repo_root,
            admission_holds,
            held,
            drain_released: &drain_released,
            claims: &claims,
            in_flight: &in_flight.footprints,
            reservations: &reservations,
        };
        let mut receipt = AdmissionReceipt {
            schema_version: 1,
            request: request.clone(),
            machine_id: identity.location().machine_id.clone(),
            claim: None,
            task: None,
            invalid_candidates: Vec::new(),
            deferred_conflicts: Vec::new(),
            crew_unavailable: Vec::new(),
            os_unavailable: Vec::new(),
            queue_depth: snapshot
                .backlog
                .iter()
                .filter(|task| screen.queued(task, &snapshot.statuses))
                .count(),
        };
        let key = receipt_key(&receipt.machine_id, &request.request_id)?;
        // A stopped landing's repair is nearly finished work for its task, so
        // it is offered before any backlog candidate [ORB-14261].
        if self.admit_repair_locked(
            identity,
            request,
            &key,
            orbit_dir,
            &in_flight.tasks,
            &mut receipt,
        )? {
            return self.lookup_admission(identity, &request.request_id);
        }
        let translator = TaskV2Store::new(self.registry.clone(), self.workspace_id.clone());
        for selected in &snapshot.backlog {
            // A task the selection already rules out is reported from it; a
            // stale reason only defers the task to the next request.
            if screen
                .footprint(selected, &snapshot.statuses, &mut receipt)
                .is_none()
            {
                continue;
            }
            // Only a survivor is re-read, and every fact is judged again on
            // what this section reads.
            let Some(bundle) = self.bundle_store.read_bundle_if_settled(&selected.id)? else {
                receipt.queue_depth = receipt.queue_depth.saturating_sub(1);
                continue;
            };
            let comments = bundle
                .comments
                .iter()
                .map(|comment| TaskComment {
                    at: comment.at,
                    by: comment.by.clone(),
                    message: comment.body.clone(),
                })
                .collect::<Vec<_>>();
            let history = task_history_from_events(bundle.events.clone());
            let task = translator.task_from_bundle(bundle)?;
            let statuses = self
                .dependency_statuses(task.dependencies().into_iter().collect(), &BTreeMap::new())?;
            if !screen.queued(&task, &statuses) {
                receipt.queue_depth = receipt.queue_depth.saturating_sub(1);
            }
            if task.status != TaskStatus::Backlog {
                continue;
            }
            let Some(footprint) = screen.footprint(&task, &statuses, &mut receipt) else {
                continue;
            };
            if let Some(reason) = validation_hold(&task, &comments, &history)? {
                receipt.queue_depth = receipt.queue_depth.saturating_sub(1);
                receipt.deferred_conflicts.push(AdmissionDiagnostic {
                    task_id: task.id.clone(),
                    reason,
                    blocked_by: Vec::new(),
                });
                continue;
            }
            let claim_id = format!("claim-{}", digest(&(&self.workspace_id, &key))?);
            let machine_id = &identity.location().machine_id;
            let offer = self.candidate_offer(&task, machine_id)?;
            let resume_candidate = offer
                .as_ref()
                .filter(|offer| offer.fresh.is_none())
                .map(|offer| offer.candidate.clone());
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
                    stored_files: footprint.clone(),
                    actor: receipt.machine_id.clone(),
                    ttl_seconds: ADMISSION_RESERVATION_TTL_SECONDS,
                    owner_run_id: None,
                    owner_metadata_json: Some(encode(&serde_json::json!({"claim_id":claim_id}))?),
                }),
                // [ORB-14338] A kept candidate this claim cannot resume is
                // never dropped silently: the task's history says why.
                append_history: offer
                    .as_ref()
                    .and_then(|offer| fresh_offer_history(offer, &claim_id, machine_id))
                    .into_iter()
                    .collect(),
                rows: vec![
                    row(RECEIPT_KIND, &key, &())?,
                    row(CLAIM_KIND, &claim_id, &())?,
                ],
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
                    repair: None,
                };
                let mut admitted = receipt.clone();
                admitted.claim = Some(claim.clone());
                admitted.task = Some(AdmissionTaskSummary {
                    id: task.id.clone(),
                    title: task.title.clone(),
                    complexity: task.complexity,
                    crew: task.crew.clone(),
                    context_files: footprint.clone(),
                    resume_candidate: resume_candidate.clone(),
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
/// version/schema, then ship mode, then before-PR review. Evaluating it returns a
/// verdict and writes nothing, so a probe and an admission cannot disagree
/// about what would be refused.
pub fn admission_refusal(
    identity: &AdmissionIdentity,
    request: &AdmissionRequest,
    owner_version: &str,
) -> Option<AdmissionRefusal> {
    if request
        .caller_fingerprint
        .as_deref()
        .is_some_and(|caller| caller != crate::contracts::distributed_drain_protocol_fingerprint())
    {
        return Some(AdmissionRefusal::ProtocolSkew);
    }
    if [
        &identity.location().machine_id,
        &request.request_id,
        &request.caller_version,
        &request.run_context.run_id,
        &request.run_context.job_name,
    ]
    .iter()
    .any(|v| v.trim().is_empty())
        || request.crews_malformed()
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
        || !request.ship.review_contract_consistent()
    {
        return Some(AdmissionRefusal::InvalidInput);
    }
    if request.caller_schema != DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA {
        return Some(AdmissionRefusal::ProtocolMismatch);
    }
    if request.caller_version != owner_version {
        return Some(AdmissionRefusal::VersionMismatch);
    }
    if !matches!(request.ship.mode.as_str(), "pr" | "local")
        || (identity.is_remote() && request.ship.mode == "local")
    {
        return Some(AdmissionRefusal::ShipModeUnsupported);
    }
    // An owner that captured `review.before_pr` admits only a leaf that runs
    // the gate its ship contract carries, so nothing is delivered unreviewed
    // [ORB-13908]. The claim's captured contract decides: the executor's own
    // switch never gates a claimed leaf. Only the PR route runs the gate.
    // After-landing review is owner-side and never refuses.
    if request.ship.before_pr && !(request.review_gate && request.ship.mode == "pr") {
        return Some(AdmissionRefusal::BeforePrUnsupported);
    }
    None
}

fn validate_request(
    identity: &AdmissionIdentity,
    request: &AdmissionRequest,
    version: &str,
) -> Result<(), OrbitError> {
    match admission_refusal(identity, request, version) {
        Some(AdmissionRefusal::ProtocolSkew) => Err(OrbitError::ProtocolSkew(format!(
            "caller fingerprint {}; owner fingerprint {}",
            request
                .caller_fingerprint
                .as_deref()
                .unwrap_or("unavailable"),
            crate::contracts::distributed_drain_protocol_fingerprint(),
        ))),
        // A caller that sends no fingerprint predates fingerprint negotiation.
        // Its build classifies only `invalid_input` as an owner refusal, so it
        // keeps that code; `protocol_skew` would read as a transport failure
        // and leave its request open.
        Some(AdmissionRefusal::ProtocolMismatch) if request.caller_fingerprint.is_none() => {
            Err(OrbitError::InvalidInput(format!(
                "protocol_mismatch: caller revision {}; owner revision {}",
                request.caller_schema, DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA
            )))
        }
        Some(AdmissionRefusal::ProtocolMismatch) => Err(OrbitError::ProtocolSkew(format!(
            "caller revision {}; owner revision {}",
            request.caller_schema, DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA
        ))),
        Some(refusal) => Err(OrbitError::InvalidInput(refusal.as_str().into())),
        None => Ok(()),
    }
}
