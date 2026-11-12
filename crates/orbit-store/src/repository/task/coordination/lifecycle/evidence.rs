use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::fs::io::atomic_write_bytes;
use orbit_common::security::release::sha256_hex;
use orbit_types::task::{
    ArtifactManifestFileV2, ArtifactManifestV2, TASK_ARTIFACT_SCHEMA_VERSION,
    TASK_ARTIFACTS_DIR_NAME, TASK_COMMENTS_FILE_NAME, TaskArtifact, TaskCommentRowV2, TaskRelation,
    TaskRelationType, push_external_ref_if_missing,
};
use orbit_types::workflow::REVIEW_REPORT_HISTORY_ARTIFACT;
use orbit_types::workflow::automation::{
    COVERAGE_ARTIFACT, ClaimedSubmission, EVIDENCE_AUTHORITY_ARTIFACT, EvidenceSubmission,
};

use super::super::{TaskCommitBoundary, TaskCommitIntent};
use super::invalid;
use crate::contracts::{ClaimCommitEffects, ClaimEvidence, ClaimRun};
use crate::driver::file::task_bundle::truncate_jsonl_file;
use crate::repository::task::v2::{normalize_v2_artifact_path, review_report_history};
use crate::repository::task::v2_bundle::{TaskBundleV2, TaskDocumentV2};

impl TaskCommitBoundary {
    /// Measure the failure's committed task, while its bundle is still locked,
    /// and stamp the receipt rows written by that same journal transaction.
    pub(in super::super) fn stamp_failure_revision(
        &self,
        rows: &mut [crate::contracts::TaskCoordinationRow],
        bundle: &TaskBundleV2,
        intent: &TaskCommitIntent,
    ) -> Result<(), OrbitError> {
        let mut failed = bundle.clone();
        failed.envelope = intent.envelope.clone();
        failed.events.extend(intent.events.iter().cloned());
        failed
            .comments
            .extend(intent.evidence.comments.iter().cloned());
        if let Some(plan) = &intent.evidence.plan {
            failed.plan = plan.clone();
        }
        let comments = failed
            .comments
            .iter()
            .cloned()
            .map(crate::repository::task::v2::task_comment_from_row)
            .collect::<Vec<_>>();
        let history = crate::repository::task::v2::task_history_from_events(failed.events.clone());
        let task = crate::repository::task::v2::TaskV2Store::new(
            self.registry.clone(),
            self.workspace_id.clone(),
        )
        .task_from_bundle(failed)?;
        let revision = crate::contracts::FinalRecoveryTaskRevision::of(&task, &comments, &history);
        for row in rows
            .iter_mut()
            .filter(|row| row.kind == super::codec::RECEIPT)
        {
            let mut receipt: super::MutationReceipt = super::decode(&row.payload_json)?;
            receipt.result.failure_revision = Some(revision.clone());
            row.payload_json = super::encode(&receipt)?;
        }
        Ok(())
    }

    pub(in super::super) fn prepare_claim_evidence(
        &self,
        intent: &mut TaskCommitIntent,
        bundle: &TaskBundleV2,
        evidence: &ClaimEvidence,
        binding: Option<&ClaimRun>,
        actor: &str,
        effects: &ClaimCommitEffects,
    ) -> Result<(), OrbitError> {
        // Only a host update may publish this record; release/failure
        // payloads are ordinary worker claims, not gate publications.
        if evidence.review_settlement.is_some()
            && (effects.worker_update.is_none() || evidence.comment.is_none())
        {
            return Err(invalid(
                "review settlement metadata requires a host verdict comment",
            ));
        }
        let origin = effects.execution_origin.as_ref();
        if let Some(update) = &effects.worker_update {
            intent.evidence.plan = update.plan.clone();
            if let Some(context) = &update.context_files {
                intent.envelope.context_files = context.clone();
            }
            for reference in &update.external_refs {
                push_external_ref_if_missing(&mut intent.envelope.external_refs, reference.clone());
            }
            intent.envelope.validate()?;
        }
        for target in &effects.regression_from {
            // An existing link, classified or not, already records this culprit.
            if !intent.envelope.relations.iter().any(|relation| {
                relation.relation_type == TaskRelationType::RegressionFrom
                    && relation.target == *target
            }) {
                intent.envelope.relations.push(TaskRelation {
                    relation_type: TaskRelationType::RegressionFrom,
                    target: target.clone(),
                    regression: None,
                });
            }
        }
        if !effects.regression_from.is_empty() {
            intent.envelope.validate()?;
        }
        if let Some(run) = binding {
            intent.envelope.job_run_id = Some(run.run_id.clone());
            // A machine's display name never participates in ownership checks.
            intent.envelope.job_run_machine = origin.cloned();
        }
        intent.evidence.summary = evidence.summary.clone();
        let comments: Vec<(&str, &String)> = evidence
            .comment
            .iter()
            .map(|message| (actor, message))
            // An observation is automation's record, whoever settles the
            // claim: its author must never read as operator steering.
            .chain(
                effects
                    .observation_comments
                    .iter()
                    .map(|message| ("system", message)),
            )
            .collect();
        if !comments.is_empty() {
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
            for (offset, (by, message)) in comments.into_iter().enumerate() {
                let comment = TaskCommentRowV2 {
                    schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
                    comment_id: format!("C-{:04}", next + offset),
                    at: Utc::now(),
                    by: by.into(),
                    body: message.clone(),
                    operation_id: None,
                };
                comment.validate()?;
                if offset == 0
                    && evidence.comment.is_some()
                    && let Some(settlement) = &evidence.review_settlement
                {
                    let entry = settlement.history_entry(comment.at, by);
                    let next_event = crate::repository::task::v2::sequencing::next_sequence(
                        &bundle.events,
                        "EV-",
                    ) + intent.events.len();
                    intent.events.push(orbit_types::task::TaskEventRowV2 {
                        schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
                        event_id: format!("EV-{next_event:04}"),
                        at: entry.at,
                        by: entry.by,
                        event_type: entry.event,
                        note: entry.note,
                        from_status: None,
                        to_status: None,
                        operation_id: None,
                        decision: entry.decision,
                    });
                }
                intent.evidence.comments.push(comment);
            }
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
            // A claimed reviewer's report reaches the owner here, not through
            // an ordinary update; retain its revision the same way [ORB-14192].
            // Only the worker's live update can still be corrected, so only it
            // is held to the record-id contract at submission [ORB-14370].
            if evidence.artifacts.iter().any(|artifact| {
                normalize_v2_artifact_path(&artifact.path).ok().as_deref()
                    == Some(REVIEW_REPORT_HISTORY_ARTIFACT)
            }) {
                return Err(invalid("review report history is reserved"));
            }
            let history = review_report_history(
                &self.bundle_store.bundle_path(&intent.task_id)?,
                &files,
                &evidence.artifacts,
                actor,
                Utc::now(),
                effects.worker_update.is_some(),
            )?;
            let mut coverage = None;
            for artifact in &evidence.artifacts {
                match normalize_v2_artifact_path(&artifact.path)?.as_str() {
                    EVIDENCE_AUTHORITY_ARTIFACT => {
                        return Err(invalid("automation evidence authority is reserved"));
                    }
                    COVERAGE_ARTIFACT => coverage = Some(&artifact.content),
                    _ => {}
                }
            }
            let witness = coverage
                .zip(effects.claimed_submission.as_ref())
                .map(|(content, submission)| claimed_witness(&intent.task_id, content, submission))
                .transpose()?;
            let mut stored = Vec::with_capacity(evidence.artifacts.len() + 2);
            let mut seen_in_request = BTreeSet::new();
            for artifact in evidence.artifacts.iter().chain(&history).chain(&witness) {
                // Same canonical contract as an ordinary artifact write, resolved
                // before the journal decision. `validate_relative_artifact_path`
                // accepts `notes/`, `a//b`, and surrounding whitespace because
                // `Path` components drop them; storing that raw string records a
                // blob the atomic write does not create.
                let path = normalize_v2_artifact_path(&artifact.path)?;
                if !seen_in_request.insert(path.clone()) {
                    return Err(invalid("duplicate artifact path"));
                }
                let aliases: Vec<String> = files
                    .keys()
                    .filter(|existing| {
                        normalize_v2_artifact_path(existing).ok().as_deref() == Some(path.as_str())
                    })
                    .cloned()
                    .collect();
                for alias in aliases {
                    files.remove(&alias);
                }
                let digest = sha256_hex(&artifact.content);
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
                        // A claimed worker authored these bytes; its claim
                        // grants no trusted writer class.
                        writer: None,
                    },
                );
                stored.push(TaskArtifact {
                    path,
                    media_type: artifact.media_type.clone(),
                    content: artifact.content.clone(),
                    created_by: artifact.created_by.clone(),
                });
            }
            let manifest = ArtifactManifestV2 {
                schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
                files: files.into_values().collect(),
            };
            manifest.validate()?;
            // `Path` equality merges `a//b` with `a/b`, so the ancestor set
            // cannot see duplicate aliases. Compare canonical strings first.
            let mut canonical_paths = Vec::with_capacity(manifest.files.len());
            let mut canonical_seen = BTreeSet::new();
            for file in &manifest.files {
                let path = normalize_v2_artifact_path(&file.path)?;
                if !canonical_seen.insert(path.clone()) {
                    return Err(invalid("duplicate artifact path"));
                }
                canonical_paths.push(path);
            }
            let paths: BTreeSet<_> = canonical_paths
                .iter()
                .map(|path| Path::new(path.as_str()))
                .collect();
            for path in &paths {
                if path
                    .ancestors()
                    .skip(1)
                    .any(|parent| paths.contains(parent))
                {
                    return Err(invalid("artifact file conflicts with an ancestor artifact"));
                }
            }
            let root = self
                .bundle_store
                .bundle_path(&intent.task_id)?
                .join(TASK_ARTIFACTS_DIR_NAME)
                .join("files");
            for artifact in &stored {
                let destination = root.join(&artifact.path);
                // Walk top-down so an existing file ancestor is refused before
                // attempting metadata on its impossible children. No directories
                // or bytes are created until after the journal decision.
                for path in destination
                    .ancestors()
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                {
                    match std::fs::metadata(path) {
                        Ok(metadata) => {
                            let compatible = if path == destination {
                                metadata.is_file()
                            } else {
                                metadata.is_dir()
                            };
                            if !compatible {
                                return Err(invalid(
                                    "artifact destination conflicts with an existing file or directory",
                                ));
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error.into()),
                    }
                }
            }
            intent.evidence.manifest = Some(manifest);
            intent.evidence.artifacts = stored;
        }
        Ok(())
    }

    pub(in super::super) fn apply_claim_evidence(
        &self,
        intent: &TaskCommitIntent,
    ) -> Result<(), OrbitError> {
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
            self.bundle_store
                .append_comments(id, &intent.evidence.comments)?;
        }
        for artifact in &intent.evidence.artifacts {
            let destination = root
                .join(TASK_ARTIFACTS_DIR_NAME)
                .join("files")
                .join(&artifact.path);
            if let Some(parent) = destination.parent() {
                orbit_common::fs::io::create_private_dir_all(parent)?;
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

/// [ORB-15962] The owner's witness for a claimed worker's coverage bytes,
/// committed in the same journal intent as those bytes. It binds their digest
/// to the task, the claim the owner authenticated and the follower run it is
/// bound to, so settlement can verify the submission after the claim lands
/// and its mutation receipts are pruned, without the follower's run record.
fn claimed_witness(
    task_id: &str,
    content: &[u8],
    (claim_id, run): &(String, ClaimRun),
) -> Result<TaskArtifact, OrbitError> {
    let witness = EvidenceSubmission {
        action_id: task_id.into(),
        evidence_digest: sha256_hex(content),
        run_id: run.run_id.clone(),
        claim: Some(ClaimedSubmission {
            claim_id: claim_id.clone(),
            machine_id: run.machine_id.clone(),
        }),
    };
    Ok(TaskArtifact {
        path: EVIDENCE_AUTHORITY_ARTIFACT.into(),
        media_type: "application/json".into(),
        content: serde_json::to_vec(&witness).map_err(|e| OrbitError::Store(e.to_string()))?,
        created_by: None,
    })
}
