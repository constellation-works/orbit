//! Admission decision: the admit entry points and the exclusive section that
//! decides each candidate.

use std::collections::BTreeMap;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::task::TaskStatus;

use super::super::TaskCommitBoundary;
use super::super::lifecycle::fresh_offer_history;
use super::super::selection::{AdmissionSnapshot, DependencyReach, Screen};
use super::{
    ADMISSION_RESERVATION_TTL_SECONDS, AdmissionGuidanceCapture, CLAIM_KIND, RECEIPT_KIND,
    StoredReceipt, ValidationHold, admission_refusal, decode, digest, encode, receipt_key, row,
};
use crate::contracts::*;
use crate::repository::task::v2::{TaskV2Store, task_comment_from_row, task_history_from_events};

impl TaskCommitBoundary {
    /// One internal owner admission. Identity and resolved ship authority are
    /// trusted arguments supplied after authorization, never payload identity.
    /// No local run, worktree, branch or public pull endpoint is created here.
    ///
    /// Candidates are selected from the generated index before the exclusive
    /// section, which stalls the partition's task writes [ORB-14724]. The
    /// section re-reads only what one decision rests on: the candidate, its
    /// dependencies, the in-flight tasks whose footprints can conflict, claims
    /// and reservations. A candidate that changed after selection is judged on
    /// what the section reads, so a stale selection can defer it but never
    /// admit it.
    ///
    /// The section is scoped to this partition, leaving other partitions'
    /// ordinary sections running [ORB-15106]. Only a candidate whose
    /// dependency another partition holds needs the host-wide section, so
    /// that dependency cannot move while the decision rests on it: the
    /// partition-scoped section stops at that read, before anything is
    /// written, and the decision is taken again host-wide.
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
    /// A footprint the owner's local drain admitted inside this same section
    /// is protected as soon as that section ends, before its gate reserves
    /// it, so a grant can never take it in between [ORB-15514].
    ///
    /// `held` maps each `backlog` task the owner is withholding for a red
    /// base to why [ORB-14258]. Its last delivery failed a required command
    /// the base fails the same way; it is deferred until the held command
    /// passes on a new base tip, as it would be on the owner's own drain.
    ///
    /// `ordering` supplies expiring frozen-batch tasks and the validation
    /// gates every receipt captures [ORB-15622]. With `repairs_only` it
    /// offers a pending landing repair and no backlog task. Candidates take
    /// local dispatch's order against `ordering.census` [ORB-17304]: within
    /// a band (or a priority, per `ordering.affinity`), work only this
    /// executor can run leads, and among the rest work fewer live executors
    /// can run. A remote requester's census row is upserted in the same
    /// commit as its receipt; the admitted receipt records the task's
    /// affinity class and the census it was ordered against.
    #[allow(clippy::too_many_arguments)]
    pub fn admit_task(
        &self,
        identity: &AdmissionIdentity,
        request: &AdmissionRequest,
        ordering: &AdmissionOrdering,
        owner_version: &str,
        repo_root: &Path,
        orbit_dir: &Path,
        admission_holds: &BTreeMap<String, String>,
        held: &BTreeMap<String, String>,
        validation_hold: &ValidationHold<'_>,
    ) -> Result<AdmissionLookup, OrbitError> {
        self.admit_task_with_guidance(
            identity,
            request,
            ordering,
            owner_version,
            repo_root,
            orbit_dir,
            admission_holds,
            held,
            validation_hold,
            &|task| {
                Ok(AdmissionTaskGuidance {
                    experiments: orbit_types::workflow::CapturedExperiments {
                        task_id: task.id.clone(),
                        assignments: Vec::new(),
                    },
                    explore_brief: None,
                    artifacts: Vec::new(),
                    visual_review: None,
                })
            },
        )
    }

    /// Admission with the owner's task-guidance callback, before the
    /// receipt is written. The callback must not acquire task admission locks.
    #[allow(
        clippy::too_many_arguments,
        reason = "admission borrows identity, ordering, holds and guidance together so the receipt records one snapshot"
    )]
    pub fn admit_task_with_guidance(
        &self,
        identity: &AdmissionIdentity,
        request: &AdmissionRequest,
        ordering: &AdmissionOrdering,
        owner_version: &str,
        repo_root: &Path,
        orbit_dir: &Path,
        admission_holds: &BTreeMap<String, String>,
        held: &BTreeMap<String, String>,
        validation_hold: &ValidationHold<'_>,
        guidance_capture: &AdmissionGuidanceCapture<'_>,
    ) -> Result<AdmissionLookup, OrbitError> {
        validate_request(identity, request, owner_version)?;
        let mut snapshot = self.admission_snapshot(ordering)?;
        if ordering.repairs_only {
            snapshot.backlog.clear();
        }
        #[cfg(test)]
        super::super::selection::after_selection::run();
        let census = identity.is_remote().then(|| ExecutorCensusEntry {
            machine_id: identity.location().machine_id.clone(),
            capability: ExecutorCapability::of_request(request),
            run_id: request.run_context.run_id.clone(),
            seen_at: chrono::Utc::now(),
        });
        let admit = |reach| {
            self.admit_locked(
                identity,
                request,
                repo_root,
                orbit_dir,
                &snapshot,
                admission_holds,
                held,
                validation_hold,
                guidance_capture,
                ordering,
                census.as_ref(),
                reach,
            )
        };
        if let Some(lookup) = self.with_admission(|| admit(DependencyReach::Partition))? {
            return Ok(lookup);
        }
        self.with_host_admission(|| admit(DependencyReach::Host))?
            .ok_or_else(|| OrbitError::Store("a host-wide admission refused a dependency".into()))
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
        guidance_capture: &AdmissionGuidanceCapture<'_>,
        ordering: &AdmissionOrdering,
        census: Option<&ExecutorCensusEntry>,
        reach: DependencyReach,
    ) -> Result<Option<AdmissionLookup>, OrbitError> {
        let executor_host_os = &ordering.executor_host_os;
        if let Some(row) = self.receipt_row(&identity.location().machine_id, &request.request_id)? {
            let previous = decode::<StoredReceipt>(&row.payload_json)?;
            let same = match previous {
                StoredReceipt::Full { receipt } => receipt.request == *request,
                StoredReceipt::Tombstone { input_digest, .. } => input_digest == digest(request)?,
            };
            if !same {
                return Err(OrbitError::InvalidInput("request_mismatch".into()));
            }
            return self
                .lookup_admission(identity, &request.request_id)
                .map(Some);
        }
        let in_flight = self.in_flight_locked(snapshot, repo_root)?;
        let claims = self.execution_claims()?;
        let drain_released =
            self.drain_releases(&identity.location().machine_id, &request.run_context.run_id)?;
        let reservations = self.store.inspect_active_task_reservations(
            &orbit_dir.to_string_lossy(),
            Some(&self.workspace_id),
        )?;
        let owner_wave = self.owner_wave_footprints_locked()?;
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
            owner_wave: &owner_wave,
            lock_modes: self.store.footprint_lock_modes(),
            executor_host_os,
            census,
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
            browser_kit_unavailable: Vec::new(),
            labels_unavailable: Vec::new(),
            capacity_unavailable: Vec::new(),
            queue_depth: snapshot
                .backlog
                .iter()
                .filter(|task| screen.queued(task, &snapshot.statuses))
                .count(),
            gates: ordering.gates.clone(),
            tag_instructions: ordering.tag_instructions.clone(),
            experiments: None,
            explore_brief: None,
            visual_review: None,
            affinity: None,
        };
        let key = receipt_key(&receipt.machine_id, &request.request_id)?;
        // A stopped landing's repair is nearly finished work for its task, so
        // it is offered before any backlog candidate [ORB-14261].
        if self.admit_repair_locked(
            &screen,
            &key,
            orbit_dir,
            &in_flight.tasks,
            &mut receipt,
            guidance_capture,
        )? {
            return self
                .lookup_admission(identity, &request.request_id)
                .map(Some);
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
                .cloned()
                .map(task_comment_from_row)
                .collect::<Vec<_>>();
            let history = task_history_from_events(bundle.events.clone());
            let task = translator.task_from_bundle(bundle)?;
            // A dependency another partition holds is out of a
            // partition-scoped section's reach. Nothing has been written yet,
            // so the caller takes this decision again host-wide.
            let Some(statuses) = self.dependency_statuses(
                task.dependencies().into_iter().collect(),
                &BTreeMap::new(),
                reach,
            )?
            else {
                return Ok(None);
            };
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
            let offer = self.admission_offer(&task, &history, machine_id)?;
            let resume_candidate = offer
                .as_ref()
                .filter(|offer| offer.fresh.is_none())
                .map(|offer| offer.candidate.clone());
            let guidance = guidance_capture(&task)?;
            receipt.experiments = Some(guidance.experiments);
            receipt.explore_brief = guidance.explore_brief;
            let guidance_evidence = ClaimEvidence {
                artifacts: guidance.artifacts,
                ..ClaimEvidence::default()
            };
            receipt.visual_review = guidance.visual_review;
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
            let effects = ClaimCommitEffects {
                census: census.cloned(),
                ..Default::default()
            };
            let committed = self.commit_locked_effects(
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
                        reservation_expires_at: reserved.expires_at.clone().ok_or_else(|| {
                            OrbitError::Store("reservation expiry missing".into())
                        })?,
                        repair: None,
                    };
                    let mut admitted = receipt.clone();
                    admitted.claim = Some(claim.clone());
                    admitted.task = Some(AdmissionTaskSummary {
                        id: task.id.clone(),
                        title: task.title.clone(),
                        complexity: task.complexity,
                        crew: task.crew.clone(),
                        crew_source: task.crew_source.clone(),
                        context_files: footprint.clone(),
                        resume_candidate: resume_candidate.clone(),
                    });
                    admitted.queue_depth = admitted.queue_depth.saturating_sub(1);
                    admitted.affinity = ordering
                        .census
                        .as_ref()
                        .map(|census| census.record(&task, executor_host_os, ordering.affinity));
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
                },
                &effects,
                &guidance_evidence,
                None,
            )?;
            if !matches!(committed, TaskCoordinationCommitOutcome::Committed(_)) {
                return Err(OrbitError::Store(
                    "admission changed inside the serialization boundary".into(),
                ));
            }
            return self
                .lookup_admission(identity, &request.request_id)
                .map(Some);
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
            census,
        )?;
        self.lookup_admission(identity, &request.request_id)
            .map(Some)
    }
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
