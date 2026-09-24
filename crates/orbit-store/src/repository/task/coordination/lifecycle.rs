//! Internal attempt lifecycle on the existing task commit journal. No public pull route.
use std::collections::BTreeMap;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::fs::io::{atomic_write_bytes, with_shared_file_lock};
use orbit_types::task::{
    ArtifactManifestFileV2, ArtifactManifestV2, TASK_ARTIFACT_SCHEMA_VERSION,
    TASK_ARTIFACTS_DIR_NAME, TASK_COMMENTS_FILE_NAME, TaskCommentRowV2, TaskStatus,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{COORDINATION_LOCK_LABEL, TaskCommitBoundary, TaskCommitIntent};
use crate::contracts::*;
use crate::driver::file::task_bundle::truncate_jsonl_file;
use crate::repository::task::v2_bundle::{TaskBundleV2, TaskDocumentV2};

const CLAIM: &str = "distributed-execution-claim-v1";
const STATE: &str = "distributed-claim-lifecycle-v1";
const RECEIPT: &str = "distributed-claim-mutation-v1";

pub(super) fn invalid(message: &str) -> OrbitError {
    OrbitError::InvalidInput(message.into())
}
pub(super) fn encode<T: Serialize>(value: &T) -> Result<String, OrbitError> {
    serde_json::to_string(value).map_err(|e| OrbitError::Store(e.to_string()))
}
pub(super) fn decode<T: serde::de::DeserializeOwned>(value: &str) -> Result<T, OrbitError> {
    serde_json::from_str(value).map_err(|e| OrbitError::Store(e.to_string()))
}
pub(super) fn row<T: Serialize>(
    kind: &str,
    id: &str,
    value: &T,
) -> Result<TaskCoordinationRow, OrbitError> {
    Ok(TaskCoordinationRow {
        kind: kind.into(),
        row_id: id.into(),
        payload_json: encode(value)?,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MutationReceipt {
    input: String,
    result: ClaimMutationResult,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(super) struct EvidenceIntent {
    summary: Option<String>,
    #[serde(default)]
    plan: Option<String>,
    comments_len: Option<u64>,
    comments: Vec<TaskCommentRowV2>,
    artifacts: Vec<orbit_types::task::TaskArtifact>,
    manifest: Option<ArtifactManifestV2>,
}

impl TaskCommitBoundary {
    /// Strictly read-only: an interrupted commit requires explicit journal recovery
    /// or an ordinary operational read first. Inspection never performs that repair.
    pub fn inspect_execution_claims(&self) -> Result<Vec<ClaimInspection>, OrbitError> {
        with_shared_file_lock(&self.host_lock_target(), COORDINATION_LOCK_LABEL, || {
            with_shared_file_lock(&self.lock_target(), COORDINATION_LOCK_LABEL, || {
                if self.pending_marker_path().try_exists()? {
                    return Err(invalid(
                        "claim inspection unavailable until pending commit is recovered",
                    ));
                }
                self.claim_states_locked()
            })
        })
    }

    /// [ORB-12575] The ordinary-participant counterpart of
    /// [`Self::inspect_execution_claims`]: the same claim states, read inside
    /// the boundary so an interrupted commit is replayed first exactly as every
    /// other runtime read does. Live commands that merely consult claims (job
    /// resume) take this route; `orbit doctor` keeps the non-repairing read.
    pub fn resolve_execution_claims(&self) -> Result<Vec<ClaimInspection>, OrbitError> {
        self.enter_ordinary(|| self.claim_states_locked())
    }

    /// Caller holds the boundary and has already settled or excluded a
    /// pending commit.
    fn claim_states_locked(&self) -> Result<Vec<ClaimInspection>, OrbitError> {
        self.store
            .task_coordination_rows(&self.workspace_id, CLAIM)?
            .iter()
            .map(|r| {
                let claim = decode(&r.payload_json)?;
                self.claim_state(claim)
            })
            .collect()
    }

    fn claim_state(&self, claim: ExecutionClaim) -> Result<ClaimInspection, OrbitError> {
        let existing =
            self.store
                .task_coordination_row(&self.workspace_id, STATE, &claim.claim_id)?;
        let mut state = match existing {
            Some(row) => decode::<ClaimInspection>(&row.payload_json)?,
            None => {
                let created = chrono::DateTime::parse_from_rfc3339(&claim.reservation_expires_at)
                    .map_err(|e| OrbitError::Store(e.to_string()))?
                    - chrono::Duration::seconds(
                        super::admission::ADMISSION_RESERVATION_TTL_SECONDS.into(),
                    );
                ClaimInspection {
                    claim: claim.clone(),
                    bound_run: None,
                    created_at: created.to_rfc3339(),
                    updated_at: created.to_rfc3339(),
                    last_event: "claimed".into(),
                    age_seconds: None,
                    unresolved_merge_intent: None,
                    landing_invalidated: false,
                }
            }
        };
        state.claim = claim;
        state.age_seconds = chrono::DateTime::parse_from_rfc3339(&state.created_at)
            .ok()
            .map(|created| {
                (Utc::now() - created.with_timezone(&Utc))
                    .num_seconds()
                    .max(0)
            });
        Ok(state)
    }

    /// Trusted-context seam for application callers and the later generic tool adapter.
    /// Missing invocation context is always refused, even for an unclaimed task.
    pub fn mutate_execution_claim(
        &self,
        invocation: Option<&ClaimInvocation>,
        mutation_id: &str,
        mutation: &ClaimMutation,
    ) -> Result<ClaimMutationResult, OrbitError> {
        let auth = invocation.ok_or_else(|| invalid("claim invocation context required"))?;
        if [mutation_id, &auth.task_id, &auth.claim_id, &auth.machine_id]
            .iter()
            .any(|s| s.trim().is_empty())
        {
            return Err(invalid("invalid claim invocation"));
        }
        self.with_admission(|| self.mutate_claim_locked(auth, mutation_id, mutation))
    }

    fn mutate_claim_locked(
        &self,
        auth: &ClaimInvocation,
        mutation_id: &str,
        mutation: &ClaimMutation,
    ) -> Result<ClaimMutationResult, OrbitError> {
        let mut identity = mutation.clone();
        if let ClaimMutation::Friction(params) = &mut identity {
            params.created_at = chrono::DateTime::UNIX_EPOCH;
        }
        let input = encode(&(
            &auth.task_id,
            &auth.claim_id,
            &auth.machine_id,
            &auth.run,
            auth.operator,
            &identity,
        ))?;
        let receipt_id = format!(
            "{:x}",
            Sha256::digest(encode(&(&auth.claim_id, mutation_id))?)
        );
        if let Some(receipt) = self.coordination_row(RECEIPT, &receipt_id)? {
            let receipt: MutationReceipt = decode(&receipt.payload_json)?;
            if receipt.input != input {
                return Err(invalid("mutation_mismatch"));
            }
            // A persisted send intent is uncertainty, never permission to send again.
            // The landing consumer must reconcile external state, including after a
            // later revocation or a changed candidate. Do not replay launch authority.
            if matches!(
                mutation,
                ClaimMutation::MergeIntent {
                    resolved: false,
                    ..
                }
            ) {
                return Err(invalid("merge intent replay requires reconciliation"));
            }
            // Binding is also a launch gate. Never replay historical launch permission
            // after handoff, failure, or revocation; other receipts are advisory outcomes.
            if let ClaimMutation::Bind { run, .. } = mutation {
                let current = self
                    .execution_claims()?
                    .into_iter()
                    .find(|c| c.claim_id == auth.claim_id)
                    .ok_or_else(|| invalid("stale_claim"))?;
                if current.phase != ExecutionClaimPhase::Running
                    || self.claim_state(current)?.bound_run.as_ref() != Some(run)
                {
                    return Err(invalid("stale_claim"));
                }
            }
            return self.with_friction_result(receipt.result, &receipt_id);
        }
        let old = self
            .coordination_row(CLAIM, &auth.claim_id)?
            .ok_or_else(|| invalid("stale_claim"))?;
        let claim: ExecutionClaim = decode(&old.payload_json)?;
        let recover_failed = auth.operator
            && claim.phase == ExecutionClaimPhase::Failed
            && matches!(mutation, ClaimMutation::Recover { .. });
        if claim.task_id != auth.task_id || !(claim.phase.is_unsettled() || recover_failed) {
            return Err(invalid("stale_claim"));
        }
        let mut state = self.claim_state(claim.clone())?;
        if (matches!(
            claim.phase,
            ExecutionClaimPhase::Running | ExecutionClaimPhase::HandedOff
        ) && state.bound_run.is_none())
            || (claim.phase == ExecutionClaimPhase::Claimed && state.bound_run.is_some())
        {
            return Err(invalid("claim binding is inconsistent with phase"));
        }
        let old_state = self.coordination_row(STATE, &claim.claim_id)?;
        if !auth.operator
            && (auth.machine_id != claim.executed_on.machine_id || auth.run != state.bound_run)
        {
            return Err(invalid("stale_claim"));
        }
        let bundle = self.bundle_store.read_bundle_lightweight(&claim.task_id)?;
        let current_claim = bundle
            .events
            .iter()
            .rev()
            .find(|e| e.event_type == "pulled_by")
            .and_then(|e| e.note.as_deref())
            .map(serde_json::from_str::<serde_json::Value>)
            .transpose()
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        if current_claim
            .as_ref()
            .and_then(|v| v.get("claim_id"))
            .and_then(serde_json::Value::as_str)
            != Some(claim.claim_id.as_str())
        {
            return Err(invalid("stale_claim"));
        }
        let expected_status = match claim.phase {
            ExecutionClaimPhase::HandedOff => TaskStatus::Review,
            ExecutionClaimPhase::Failed => TaskStatus::Blocked,
            ExecutionClaimPhase::Landed => TaskStatus::Done,
            _ => TaskStatus::InProgress,
        };
        if bundle.envelope.status != expected_status {
            return Err(invalid("stale_claim"));
        }
        if let Some(bound) = &state.bound_run
            && (bundle.envelope.job_run_id.as_deref() != Some(&bound.run_id)
                || bundle
                    .envelope
                    .job_run_machine
                    .as_ref()
                    .map(|location| location.machine_id.as_str())
                    != Some(bound.machine_id.as_str()))
        {
            return Err(invalid("stale_claim"));
        }
        let mut params = TaskCoordinationCommitParams {
            task_id: claim.task_id.clone(),
            actor: auth.machine_id.clone(),
            expected_status: vec![expected_status],
            ..Default::default()
        };
        let mut evidence = ClaimEvidence::default();
        let mut binding = None;
        let mut release = false;
        let mut handoff_effects = ClaimCommitEffects::default();
        match mutation {
            ClaimMutation::Update(value) => {
                if auth.operator
                    || !matches!(
                        claim.phase,
                        ExecutionClaimPhase::Running | ExecutionClaimPhase::HandedOff
                    )
                    || value
                        .expected_status
                        .is_some_and(|status| status != expected_status)
                {
                    return Err(invalid("stale_claim"));
                }
                if let Some(status) = value.status {
                    if status == TaskStatus::Blocked && claim.phase == ExecutionClaimPhase::Running
                    {
                        if value
                            .evidence
                            .summary
                            .as_deref()
                            .is_none_or(|text| text.trim().is_empty())
                        {
                            return Err(invalid("failure settlement requires evidence"));
                        }
                        state.claim.phase = ExecutionClaimPhase::Failed;
                        state.landing_invalidated = true;
                        params.status = Some(status);
                        release = true;
                    } else if status != expected_status {
                        return Err(invalid(
                            "worker lifecycle transition requires typed handoff",
                        ));
                    }
                }
                if value
                    .context_files
                    .as_ref()
                    .is_some_and(|context| context != &bundle.envelope.context_files)
                {
                    return Err(invalid(
                        "claim footprint changes require recovery and readmission",
                    ));
                }
                if claim.phase == ExecutionClaimPhase::HandedOff
                    && (value.evidence != ClaimEvidence::default()
                        || value.plan.is_some()
                        || value.status_note.is_some()
                        || value
                            .external_refs
                            .iter()
                            .any(|reference| !bundle.envelope.external_refs.contains(reference)))
                {
                    return Err(invalid("stale_claim"));
                }
                evidence = value.evidence.clone();
                params.status_note = value.status_note.clone();
                handoff_effects.worker_update = Some(value.clone());
                state.last_event = "claim_updated".into();
            }
            ClaimMutation::Friction(value) => {
                if auth.operator
                    || claim.phase != ExecutionClaimPhase::Running
                    || value
                        .during_task
                        .as_ref()
                        .is_some_and(|task| task != &auth.task_id)
                {
                    return Err(invalid("stale_claim"));
                }
                let mut value = value.clone();
                value.during_task = Some(auth.task_id.clone());
                handoff_effects.friction = Some((value, receipt_id.clone()));
                state.last_event = "claim_friction".into();
            }
            ClaimMutation::Bind { run, ship } => {
                if auth.operator
                    || claim.phase != ExecutionClaimPhase::Claimed
                    || state.bound_run.is_some()
                    || run.machine_id != auth.machine_id
                    || run.run_id.trim().is_empty()
                {
                    return Err(invalid("stale_claim"));
                }
                let original = self.lookup_admission(
                    &AdmissionIdentity::trusted_local(claim.executed_on.clone()),
                    &claim.request_id,
                )?;
                let AdmissionLookup::Found { receipt, .. } = original else {
                    return Err(invalid("claim receipt unavailable"));
                };
                if receipt.request.ship != *ship {
                    return Err(invalid("claim policy mismatch"));
                }
                // One host-qualified leaf cannot belong to two attempts, even after settlement.
                if self.coordination_rows(STATE)?.iter().any(|r| {
                    decode::<ClaimInspection>(&r.payload_json)
                        .is_ok_and(|s| s.bound_run.as_ref() == Some(run))
                }) {
                    return Err(invalid("leaf run already bound"));
                }
                binding = Some(run.clone());
                state.bound_run = binding.clone();
                state.claim.phase = ExecutionClaimPhase::Running;
                state.last_event = "claim_bound".into();
            }
            ClaimMutation::Handoff(_) => return Err(invalid("typed handoff required")),
            ClaimMutation::AcceptHandoff(handoff) => {
                self.accept_typed_handoff(auth, &state, handoff, &mut params)?;
                evidence.summary = Some(handoff.execution_summary.clone());
                state.claim.phase = ExecutionClaimPhase::HandedOff;
                params.status = Some(TaskStatus::Review);
                release = true;
                state.last_event = "claim_handed_off".into();
            }
            ClaimMutation::ApproveHandoff {
                handoff_id,
                candidate,
            } => {
                self.approve_typed_handoff(auth, &state, handoff_id, candidate, &mut params)?;
                state.last_event = "handoff_approved".into();
            }
            ClaimMutation::RevokeHandoff { handoff_id, reason } => {
                if !auth.operator
                    || claim.phase != ExecutionClaimPhase::HandedOff
                    || reason.trim().is_empty()
                {
                    return Err(invalid("operator handoff revocation requires a reason"));
                }
                if state.unresolved_merge_intent.is_some() {
                    return Err(invalid("unresolved external merge intent"));
                }
                if self.accepted_handoff(&auth.claim_id)?.handoff_id != *handoff_id {
                    return Err(invalid("handoff identity mismatch"));
                }
                self.revoke_handoff_authority(auth, reason, &mut params, &mut handoff_effects)?;
                state.landing_invalidated = true;
                state.last_event = "handoff_revoked".into();
                params.status_note = Some(reason.clone());
            }
            ClaimMutation::Evidence(value) | ClaimMutation::Fail(value) => {
                if auth.operator
                    || !matches!(
                        claim.phase,
                        ExecutionClaimPhase::Claimed | ExecutionClaimPhase::Running
                    )
                {
                    return Err(invalid("stale_claim"));
                }
                evidence = value.clone();
                state.last_event = "claim_evidence".into();
                if matches!(mutation, ClaimMutation::Fail(_)) {
                    if value.summary.as_deref().is_none_or(|s| s.trim().is_empty()) {
                        return Err(invalid("failure settlement requires evidence"));
                    }
                    state.claim.phase = ExecutionClaimPhase::Failed;
                    state.landing_invalidated = true;
                    params.status = Some(TaskStatus::Blocked);
                    release = true;
                    state.last_event = "claim_failed".into();
                }
            }
            ClaimMutation::Recover { status, reason } => {
                if !auth.operator {
                    return Err(invalid("operator recovery capability required"));
                }
                if state.unresolved_merge_intent.is_some() {
                    return Err(invalid("unresolved external merge intent"));
                }
                if !matches!(status, TaskStatus::Blocked | TaskStatus::Backlog)
                    || reason.trim().is_empty()
                {
                    return Err(invalid(
                        "recovery requires a reason and blocked or backlog target",
                    ));
                }
                self.revoke_handoff_authority(auth, reason, &mut params, &mut handoff_effects)?;
                state.claim.phase = ExecutionClaimPhase::Revoked;
                state.landing_invalidated = true;
                params.status = Some(*status);
                params.status_note = Some(reason.clone());
                release = true;
                state.last_event = "claim_revoked".into();
            }
            ClaimMutation::DispatchLanding {
                handoff_id,
                job_run_id,
            } => {
                self.dispatch_landing_attempt(
                    auth,
                    &state,
                    handoff_id,
                    job_run_id.as_ref(),
                    &mut params,
                    &mut handoff_effects,
                )?;
                state.last_event = "landing_dispatched".into();
            }
            ClaimMutation::CompleteLanding {
                handoff_id,
                evidence: proof,
            } => {
                self.complete_landing_attempt(
                    auth,
                    &state,
                    handoff_id,
                    proof,
                    &mut params,
                    &mut handoff_effects,
                )?;
                state.claim.phase = ExecutionClaimPhase::Landed;
                state.last_event = "landing_completed".into();
            }
            ClaimMutation::StopLanding { handoff_id, reason } => {
                self.stop_landing_attempt(
                    auth,
                    &state,
                    handoff_id,
                    reason,
                    &mut params,
                    &mut handoff_effects,
                )?;
                state.last_event = "landing_stopped".into();
            }
            ClaimMutation::MergeIntent {
                intent_id,
                resolved,
                evidence: proof,
            } => {
                if !auth.operator
                    || claim.phase != ExecutionClaimPhase::HandedOff
                    || state.landing_invalidated
                {
                    return Err(invalid("current operator landing context required"));
                }
                if intent_id.trim().is_empty() || proof.trim().is_empty() {
                    return Err(invalid("merge intent evidence required"));
                }
                if *resolved {
                    if state.unresolved_merge_intent.as_ref() != Some(intent_id) {
                        return Err(invalid("merge intent mismatch"));
                    }
                    state.unresolved_merge_intent = None;
                } else {
                    if state.unresolved_merge_intent.is_some() {
                        return Err(invalid("merge intent already unresolved"));
                    }
                    self.check_handoff_landing(auth, &state)?;
                    state.unresolved_merge_intent = Some(intent_id.clone());
                }
                params.status_note = Some(encode(&(intent_id, resolved, proof))?);
                state.last_event = "claim_merge_intent".into();
            }
        }
        state.updated_at = Utc::now().to_rfc3339();
        state.age_seconds = None;
        params.status_event = Some(state.last_event.clone());
        let result = ClaimMutationResult {
            claim_id: claim.claim_id.clone(),
            phase: state.claim.phase,
            status: params.status.unwrap_or(expected_status),
            friction: None,
        };
        params.rows.push(row(
            RECEIPT,
            &receipt_id,
            &MutationReceipt {
                input,
                result: result.clone(),
            },
        )?);
        let new_state = row(STATE, &claim.claim_id, &state)?;
        let mut effects = ClaimCommitEffects {
            execution_origin: Some(claim.executed_on.clone()),
            worker_update: handoff_effects.worker_update,
            friction: handoff_effects.friction,
            replacements: handoff_effects.replacements,
            release_reservation: release.then_some(claim.reservation_id),
        };
        effects
            .replacements
            .push((old, row(CLAIM, &claim.claim_id, &state.claim)?));
        if let Some(old_state) = old_state {
            effects.replacements.push((old_state, new_state));
        } else {
            params.rows.push(new_state);
        }
        let outcome = self.commit_locked_effects(
            &params,
            &mut |_| Ok(params.rows.clone()),
            &effects,
            &evidence,
            binding.as_ref(),
        )?;
        match outcome {
            TaskCoordinationCommitOutcome::Committed(_) => {
                self.with_friction_result(result, &receipt_id)
            }
            _ => Err(invalid("stale_claim")),
        }
    }

    fn with_friction_result(
        &self,
        mut result: ClaimMutationResult,
        receipt_id: &str,
    ) -> Result<ClaimMutationResult, OrbitError> {
        if let Some(row) = self.coordination_row("distributed-claim-friction-v1", receipt_id)? {
            result.friction = Some(decode(&row.payload_json)?);
        }
        Ok(result)
    }

    pub(super) fn prepare_claim_evidence(
        &self,
        intent: &mut TaskCommitIntent,
        bundle: &TaskBundleV2,
        evidence: &ClaimEvidence,
        binding: Option<&ClaimRun>,
        actor: &str,
        effects: &ClaimCommitEffects,
    ) -> Result<(), OrbitError> {
        let origin = effects.execution_origin.as_ref();
        if let Some(update) = &effects.worker_update {
            intent.evidence.plan = update.plan.clone();
            if let Some(context) = &update.context_files {
                intent.envelope.context_files = context.clone();
            }
            for reference in &update.external_refs {
                if !intent.envelope.external_refs.contains(reference) {
                    intent.envelope.external_refs.push(reference.clone());
                }
            }
            intent.envelope.validate()?;
        }
        if let Some(run) = binding {
            intent.envelope.job_run_id = Some(run.run_id.clone());
            // A machine's display name never participates in ownership checks.
            intent.envelope.job_run_machine = origin.cloned();
        }
        intent.evidence.summary = evidence.summary.clone();
        if let Some(message) = &evidence.comment {
            let path = self
                .bundle_store
                .bundle_path(&intent.task_id)?
                .join(TASK_COMMENTS_FILE_NAME);
            intent.evidence.comments_len = Some(match std::fs::metadata(path) {
                Ok(m) => m.len(),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
                Err(e) => return Err(e.into()),
            });
            let next =
                crate::repository::task::v2::sequencing::next_sequence(&bundle.comments, "C-");
            let comment = TaskCommentRowV2 {
                schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
                comment_id: format!("C-{next:04}"),
                at: Utc::now(),
                by: actor.into(),
                body: message.clone(),
            };
            comment.validate()?;
            intent.evidence.comments.push(comment);
        }
        if !evidence.artifacts.is_empty() {
            let mut files: BTreeMap<_, _> = bundle
                .artifact_manifest
                .clone()
                .unwrap_or(ArtifactManifestV2 {
                    schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
                    files: vec![],
                })
                .files
                .into_iter()
                .map(|f| (f.path.clone(), f))
                .collect();
            for artifact in &evidence.artifacts {
                let path = &artifact.path;
                // Same artifact-path contract, validated before any durable decision.
                orbit_types::task::validate_relative_artifact_path(path)?;
                if path == orbit_types::workflow::automation::EVIDENCE_AUTHORITY_ARTIFACT {
                    return Err(invalid("automation evidence authority is reserved"));
                }
                let digest = format!("{:x}", Sha256::digest(&artifact.content));
                files.insert(
                    path.clone(),
                    ArtifactManifestFileV2 {
                        path: path.clone(),
                        blob: format!("files/{path}"),
                        sha256: digest,
                        media_type: artifact.media_type.clone(),
                        size_bytes: artifact.content.len() as u64,
                        created_by: actor.into(),
                        created_at: Utc::now(),
                        origin: origin.cloned(),
                    },
                );
            }
            let manifest = ArtifactManifestV2 {
                schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
                files: files.into_values().collect(),
            };
            manifest.validate()?;
            intent.evidence.manifest = Some(manifest);
            intent.evidence.artifacts = evidence.artifacts.clone();
        }
        Ok(())
    }

    pub(super) fn apply_claim_evidence(&self, intent: &TaskCommitIntent) -> Result<(), OrbitError> {
        let id = &intent.task_id;
        let root = self.bundle_store.bundle_path(id)?;
        if let Some(plan) = &intent.evidence.plan {
            self.bundle_store
                .rewrite_document(id, TaskDocumentV2::Plan, plan)?;
        }
        if let Some(summary) = &intent.evidence.summary {
            self.bundle_store
                .rewrite_document(id, TaskDocumentV2::ExecutionSummary, summary)?;
        }
        if let Some(len) = intent.evidence.comments_len {
            truncate_jsonl_file(&root.join(TASK_COMMENTS_FILE_NAME), len)?;
            for comment in &intent.evidence.comments {
                self.bundle_store.append_comment(id, comment)?;
            }
        }
        super::fail_if_injected(super::CoordinationFault::DuringEvidenceApply)?;
        for artifact in &intent.evidence.artifacts {
            let destination = root
                .join(TASK_ARTIFACTS_DIR_NAME)
                .join("files")
                .join(&artifact.path);
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent)?;
            }
            atomic_write_bytes(&destination, &artifact.content)
                .map_err(|e| OrbitError::from_write_io(&destination, e))?;
        }
        if let Some(manifest) = &intent.evidence.manifest {
            self.bundle_store.rewrite_artifact_manifest(id, manifest)?;
        }
        Ok(())
    }
}
