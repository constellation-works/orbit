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

fn invalid(message: &str) -> OrbitError {
    OrbitError::InvalidInput(message.into())
}
fn encode<T: Serialize>(value: &T) -> Result<String, OrbitError> {
    serde_json::to_string(value).map_err(|e| OrbitError::Store(e.to_string()))
}
fn decode<T: serde::de::DeserializeOwned>(value: &str) -> Result<T, OrbitError> {
    serde_json::from_str(value).map_err(|e| OrbitError::Store(e.to_string()))
}
fn row<T: Serialize>(kind: &str, id: &str, value: &T) -> Result<TaskCoordinationRow, OrbitError> {
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
                let claims = self
                    .store
                    .task_coordination_rows(&self.workspace_id, CLAIM)?;
                claims
                    .iter()
                    .map(|r| {
                        let claim = decode(&r.payload_json)?;
                        self.claim_state(claim)
                    })
                    .collect()
            })
        })
    }

    fn claim_state(&self, claim: ExecutionClaim) -> Result<ClaimInspection, OrbitError> {
        let existing = self
            .store
            .task_coordination_rows(&self.workspace_id, STATE)?
            .into_iter()
            .find(|r| r.row_id == claim.claim_id);
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
        let input = encode(&(
            &auth.task_id,
            &auth.claim_id,
            &auth.machine_id,
            &auth.run,
            auth.operator,
            mutation,
        ))?;
        let receipt_id = format!(
            "{:x}",
            Sha256::digest(encode(&(&auth.claim_id, mutation_id))?)
        );
        if let Some(receipt) = self
            .coordination_rows(RECEIPT)?
            .iter()
            .find(|r| r.row_id == receipt_id)
        {
            let receipt: MutationReceipt = decode(&receipt.payload_json)?;
            if receipt.input != input {
                return Err(invalid("mutation_mismatch"));
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
            return Ok(receipt.result);
        }
        let old = self
            .coordination_rows(CLAIM)?
            .into_iter()
            .find(|r| r.row_id == auth.claim_id)
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
        let old_state = self
            .coordination_rows(STATE)?
            .into_iter()
            .find(|r| r.row_id == claim.claim_id);
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
        let expected_status = if claim.phase == ExecutionClaimPhase::HandedOff {
            TaskStatus::Review
        } else if claim.phase == ExecutionClaimPhase::Failed {
            TaskStatus::Blocked
        } else {
            TaskStatus::InProgress
        };
        if bundle.envelope.status != expected_status {
            return Err(invalid("stale_claim"));
        }
        if let Some(bound) = &state.bound_run
            && (bundle.envelope.job_run_id.as_deref() != Some(&bound.run_id)
                || bundle
                    .envelope
                    .job_run_host
                    .as_ref()
                    .map(|h| h.machine_id.as_str())
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
        match mutation {
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
            ClaimMutation::Evidence(value)
            | ClaimMutation::Handoff(value)
            | ClaimMutation::Fail(value) => {
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
                if matches!(mutation, ClaimMutation::Handoff(_)) {
                    if claim.phase != ExecutionClaimPhase::Running
                        || value.summary.as_deref().is_none_or(|s| s.trim().is_empty())
                    {
                        return Err(invalid("handoff requires a running claim and evidence"));
                    }
                    state.claim.phase = ExecutionClaimPhase::HandedOff;
                    params.status = Some(TaskStatus::Review);
                    release = true;
                    state.last_event = "claim_handed_off".into();
                } else if matches!(mutation, ClaimMutation::Fail(_)) {
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
                state.claim.phase = ExecutionClaimPhase::Revoked;
                state.landing_invalidated = true;
                params.status = Some(*status);
                params.status_note = Some(reason.clone());
                release = true;
                state.last_event = "claim_revoked".into();
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
            replacements: vec![(old, row(CLAIM, &claim.claim_id, &state.claim)?)],
            release_reservation: release.then_some(claim.reservation_id),
        };
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
            TaskCoordinationCommitOutcome::Committed(_) => Ok(result),
            _ => Err(invalid("stale_claim")),
        }
    }

    pub(super) fn prepare_claim_evidence(
        &self,
        intent: &mut TaskCommitIntent,
        bundle: &TaskBundleV2,
        evidence: &ClaimEvidence,
        binding: Option<&ClaimRun>,
        actor: &str,
    ) -> Result<(), OrbitError> {
        if let Some(run) = binding {
            intent.envelope.job_run_id = Some(run.run_id.clone());
            // Host display labels never participate in ownership checks.
            intent.envelope.job_run_host = Some(ExecutionLocation {
                machine_id: run.machine_id.clone(),
                host_id: None,
            });
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
                        origin: Some(ExecutionLocation {
                            machine_id: actor.into(),
                            host_id: None,
                        }),
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
