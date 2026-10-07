//! Admission of the one automatic repair a stopped landing earns [ORB-14261].
//!
//! When the owner's landing stops because a handed-off candidate conflicts
//! with, or is stale against, its base, the claim moves to `repair_pending`
//! and the task back to `in-progress` (see [`super::landing`]). This module
//! hands that claim's preserved candidate to the next pull that may take it:
//! a new claim, carrying [`ClaimRepair`], whose leaf re-applies the candidate
//! onto the current base, revalidates it and hands it off again under the
//! same task. The superseded claim settles as `revoked` in the same commit.
//!
//! The original executor is preferred: it already holds the candidate's
//! objects and ran its implementation. The owner's own local drain takes the
//! repair once [`REPAIR_OWNER_FALLBACK_SECONDS`] have passed without that
//! executor pulling it. Any other follower never does.
use std::collections::BTreeSet;
use std::path::Path;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskStatus};
use orbit_types::workflow::handoff::{HandoffCandidate, HandoffDelivery};

use super::TaskCommitBoundary;
use super::admission::{
    ADMISSION_RESERVATION_TTL_SECONDS, RECEIPT_KIND, StoredReceipt, digest, os_unavailable,
};
use super::lifecycle::{CLAIM, STATE, decode, encode, row};
use crate::contracts::*;

/// How long a pending repair waits for its original executor before the
/// owner's local drain may take it.
const REPAIR_OWNER_FALLBACK_SECONDS: i64 = 30 * 60;

/// A `repair_pending` claim this pull may take, with what its repair carries.
struct RepairCandidate<'a> {
    state: ClaimInspection,
    task: &'a Task,
    repair: ClaimRepair,
}

impl TaskCommitBoundary {
    /// Admit the first pending repair this pull may take, as a new claim.
    ///
    /// Returns `true` when a repair claim was committed. Pending repairs this
    /// executor may not take, or cannot run, are reported on `receipt` and
    /// left for one that can. Caller holds the admission boundary.
    pub(super) fn admit_repair_locked(
        &self,
        identity: &AdmissionIdentity,
        request: &AdmissionRequest,
        receipt_key: &str,
        orbit_dir: &Path,
        tasks: &[Task],
        receipt: &mut AdmissionReceipt,
    ) -> Result<bool, OrbitError> {
        let candidates = self.repair_candidates(identity, request, tasks, receipt)?;
        receipt.queue_depth += candidates.len();
        for candidate in candidates {
            if self.commit_repair_claim(
                identity,
                request,
                receipt_key,
                orbit_dir,
                receipt,
                &candidate,
            )? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn repair_candidates<'a>(
        &self,
        identity: &AdmissionIdentity,
        request: &AdmissionRequest,
        tasks: &'a [Task],
        receipt: &mut AdmissionReceipt,
    ) -> Result<Vec<RepairCandidate<'a>>, OrbitError> {
        let mut candidates = Vec::new();
        for state in self.claim_states_locked()? {
            if state.claim.phase != ExecutionClaimPhase::RepairPending {
                continue;
            }
            let task_id = state.claim.task_id.clone();
            let diagnose = |reason: String| AdmissionDiagnostic {
                task_id: task_id.clone(),
                reason,
                blocked_by: Vec::new(),
            };
            let Some(task) = tasks
                .iter()
                .find(|task| task.id == task_id && task.status == TaskStatus::InProgress)
            else {
                receipt.invalid_candidates.push(diagnose(
                    "a repair is pending but the task is no longer in progress; recover the \
                     claim from the owner's operator console"
                        .into(),
                ));
                continue;
            };
            if let Some(reason) = repair_executor_wait(identity, &state) {
                receipt.deferred_conflicts.push(diagnose(reason));
                continue;
            }
            let Some(accepted) = self.find_accepted_handoff(&state.claim.claim_id)? else {
                receipt.invalid_candidates.push(diagnose(
                    "a repair is pending but its claim holds no accepted handoff".into(),
                ));
                continue;
            };
            if let Some(reason) = ship_mismatch(&accepted.handoff.candidate, &request.ship) {
                receipt.invalid_candidates.push(diagnose(reason));
                continue;
            }
            if let Some(reason) = os_unavailable(task, request) {
                receipt.os_unavailable.push(diagnose(reason));
                continue;
            }
            if let Some(reason) = request
                .crews
                .as_ref()
                .and_then(|crews| crews.unrunnable_reason(task.crew.as_deref()))
            {
                receipt.crew_unavailable.push(diagnose(reason));
                continue;
            }
            let stop_evidence = self
                .landing_attempt(&accepted.handoff_id)?
                .and_then(|attempt| attempt.evidence)
                .unwrap_or_default();
            candidates.push(RepairCandidate {
                repair: ClaimRepair {
                    repairs_claim_id: state.claim.claim_id.clone(),
                    handoff_id: accepted.handoff_id,
                    candidate: accepted.handoff.candidate,
                    stop_evidence,
                },
                state,
                task,
            });
        }
        Ok(candidates)
    }

    /// One commit: the repair claim, its receipt and reservation, the
    /// superseded claim settled as `revoked`, and a `pulled_by` event naming
    /// both. A reservation conflict defers the repair rather than failing
    /// the pull.
    fn commit_repair_claim(
        &self,
        identity: &AdmissionIdentity,
        request: &AdmissionRequest,
        receipt_key: &str,
        orbit_dir: &Path,
        receipt: &mut AdmissionReceipt,
        candidate: &RepairCandidate<'_>,
    ) -> Result<bool, OrbitError> {
        let RepairCandidate {
            state,
            task,
            repair,
        } = candidate;
        let superseded = &state.claim;
        let claim_id = format!("claim-{}", digest(&(&self.workspace_id, receipt_key))?);
        let footprint = superseded.footprint.clone();
        let machine_id = receipt.machine_id.clone();
        let old_claim = self
            .coordination_row(CLAIM, &superseded.claim_id)?
            .ok_or_else(|| OrbitError::Store("repair-pending claim row is missing".into()))?;
        let mut revoked = superseded.clone();
        revoked.phase = ExecutionClaimPhase::Revoked;
        // The new claim reserves the same footprint before the superseded
        // claim's reservation is released, in one transaction.
        let mut effects = ClaimCommitEffects {
            release_reservation: Some(superseded.reservation_id.clone()),
            ..Default::default()
        };
        effects
            .replacements
            .push((old_claim, row(CLAIM, &superseded.claim_id, &revoked)?));
        if let Some(old_state) = self.coordination_row(STATE, &superseded.claim_id)? {
            let mut settled: ClaimInspection = decode(&old_state.payload_json)?;
            settled.claim = revoked;
            settled.last_event = "repair_admitted".into();
            settled.updated_at = Utc::now().to_rfc3339();
            settled.age_seconds = None;
            effects
                .replacements
                .push((old_state, row(STATE, &superseded.claim_id, &settled)?));
        }
        let params = TaskCoordinationCommitParams {
            task_id: task.id.clone(),
            actor: machine_id.clone(),
            expected_status: vec![TaskStatus::InProgress],
            status_event: Some("pulled_by".into()),
            status_note: Some(encode(&serde_json::json!({
                "machine_id": machine_id,
                "run_context": request.run_context,
                "claim_id": claim_id,
                "request_id": request.request_id,
                "repairs_claim_id": repair.repairs_claim_id,
                "repairs_handoff_id": repair.handoff_id,
            }))?),
            reservation: Some(TaskReservationReserveParams {
                workspace_orbit_dir: orbit_dir.to_string_lossy().into_owned(),
                workspace_id: Some(self.workspace_id.clone()),
                task_ids: vec![task.id.clone()],
                requested_files: footprint.clone(),
                actor: machine_id.clone(),
                ttl_seconds: ADMISSION_RESERVATION_TTL_SECONDS,
                owner_run_id: None,
                owner_metadata_json: Some(encode(&serde_json::json!({"claim_id": claim_id}))?),
            }),
            rows: vec![
                row(RECEIPT_KIND, receipt_key, &())?,
                row(CLAIM, &claim_id, &())?,
            ],
            ..Default::default()
        };
        let outcome = self.commit_locked_effects(
            &params,
            &mut |reservation| {
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
                    repair: Some(repair.clone()),
                };
                let mut admitted = receipt.clone();
                admitted.claim = Some(claim.clone());
                admitted.task = Some(AdmissionTaskSummary {
                    id: task.id.clone(),
                    title: task.title.clone(),
                    complexity: task.complexity,
                    crew: task.crew.clone(),
                    context_files: footprint.clone(),
                    // The repair carries its own candidate in `claim.repair`.
                    resume_candidate: None,
                });
                admitted.queue_depth = admitted.queue_depth.saturating_sub(1);
                Ok(vec![
                    row(
                        RECEIPT_KIND,
                        receipt_key,
                        &StoredReceipt::Full {
                            receipt: Box::new(admitted),
                        },
                    )?,
                    row(CLAIM, &claim_id, &claim)?,
                ])
            },
            &effects,
            &ClaimEvidence::default(),
            None,
        )?;
        match outcome {
            TaskCoordinationCommitOutcome::Committed(_) => Ok(true),
            TaskCoordinationCommitOutcome::Conflicted { conflicts, .. } => {
                receipt.deferred_conflicts.push(AdmissionDiagnostic {
                    task_id: task.id.clone(),
                    reason: format!(
                        "repair footprint is reserved: {}",
                        conflicts
                            .iter()
                            .map(|conflict| format!("{} by {}", conflict.file, conflict.held_by_id))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    blocked_by: conflicts
                        .iter()
                        .map(|conflict| conflict.held_by_id.clone())
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect(),
                });
                Ok(false)
            }
            _ => Err(OrbitError::Store(
                "repair admission changed inside the serialization boundary".into(),
            )),
        }
    }
}

/// Why `identity` may not take this pending repair yet, or `None` when it may:
/// the claim's original executor always may, and the owner's local drain once
/// the fallback window has passed since the landing stopped.
fn repair_executor_wait(identity: &AdmissionIdentity, state: &ClaimInspection) -> Option<String> {
    let original = &state.claim.executed_on.machine_id;
    if identity.location().machine_id == *original {
        return None;
    }
    let waited = chrono::DateTime::parse_from_rfc3339(&state.updated_at)
        .ok()
        .map(|stopped| (Utc::now() - stopped.with_timezone(&Utc)).num_seconds());
    if !identity.is_remote() && waited.is_some_and(|waited| waited >= REPAIR_OWNER_FALLBACK_SECONDS)
    {
        return None;
    }
    Some(format!(
        "a repair of its stopped landing is reserved for its original executor {original}; the \
         owner's local drain takes it after {} minutes",
        REPAIR_OWNER_FALLBACK_SECONDS / 60
    ))
}

/// Why this pull's ship contract cannot carry the preserved candidate, or
/// `None` when it can: the same base and landing branches, and the delivery
/// route the candidate was published on.
fn ship_mismatch(candidate: &HandoffCandidate, ship: &AdmissionShipContract) -> Option<String> {
    let route = match &candidate.delivery {
        HandoffDelivery::PullRequest { .. } => "pr",
        HandoffDelivery::LocalCandidate => "local",
        HandoffDelivery::NoDiff { .. } | HandoffDelivery::AlreadyLanded { .. } => {
            return Some("a no-diff or already-landed delivery has no candidate to repair".into());
        }
    };
    (candidate.base_branch != ship.base_branch
        || candidate.landing_branch != ship.landing_branch
        || ship.mode != route)
        .then(|| {
            format!(
                "the repair needs ship mode `{route}` onto base `{}` landing on `{}`; this pull \
                 ships `{}` onto `{}` landing on `{}`",
                candidate.base_branch,
                candidate.landing_branch,
                ship.mode,
                ship.base_branch,
                ship.landing_branch
            )
        })
}
