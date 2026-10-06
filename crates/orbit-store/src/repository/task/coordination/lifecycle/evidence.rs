use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::fs::io::atomic_write_bytes;
use orbit_common::security::release::sha256_hex;
use orbit_types::task::{
    ArtifactManifestFileV2, ArtifactManifestV2, TASK_ARTIFACT_SCHEMA_VERSION,
    TASK_ARTIFACTS_DIR_NAME, TASK_COMMENTS_FILE_NAME, TaskCommentRowV2,
};
use orbit_types::workflow::REVIEW_REPORT_HISTORY_ARTIFACT;

use super::super::{TaskCommitBoundary, TaskCommitIntent};
use super::invalid;
use crate::contracts::{ClaimCommitEffects, ClaimEvidence, ClaimRun};
use crate::driver::file::task_bundle::truncate_jsonl_file;
use crate::repository::task::v2::{normalize_v2_artifact_path, review_report_history};
use crate::repository::task::v2_bundle::{TaskBundleV2, TaskDocumentV2};

impl TaskCommitBoundary {
    pub(in super::super) fn prepare_claim_evidence(
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
            // A claimed reviewer's report reaches the owner here, not through
            // an ordinary update; retain its revision the same way [ORB-14192].
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
            )?;
            let mut stored = Vec::with_capacity(evidence.artifacts.len() + 1);
            let mut seen_in_request = BTreeSet::new();
            for artifact in evidence.artifacts.iter().chain(&history) {
                // Same canonical contract as an ordinary artifact write, resolved
                // before the journal decision. `validate_relative_artifact_path`
                // accepts `notes/`, `a//b`, and surrounding whitespace because
                // `Path` components drop them; storing that raw string records a
                // blob the atomic write does not create.
                let path = normalize_v2_artifact_path(&artifact.path)?;
                if path == orbit_types::workflow::automation::EVIDENCE_AUTHORITY_ARTIFACT {
                    return Err(invalid("automation evidence authority is reserved"));
                }
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
                    },
                );
                stored.push(orbit_types::task::TaskArtifact {
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
            for comment in &intent.evidence.comments {
                self.bundle_store.append_comment(id, comment)?;
            }
        }
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
