use super::*;

use orbit_types::workflow::{
    REVIEW_REPORT_ARTIFACT, REVIEW_REPORT_HISTORY_ARTIFACT, ReviewReport, ReviewReportHistory,
    ReviewReportRevision,
};

/// The report history to write beside a before-PR review report this update
/// replaces [ORB-14192]: the held history with the new revision appended.
///
/// Both artifact writers call it — an ordinary update and a claimed worker's
/// evidence committed on the owner. It runs under the task lock and lands in
/// the same manifest rewrite as the report, so no accepted revision can be replaced before it is retained —
/// whether or not settlement ran in between — and a crash leaves either both
/// or neither. A report that does not parse carries no obligations and adds
/// no revision; settlement refuses it on its own. A held history this build
/// cannot read refuses the update rather than being overwritten.
pub(crate) fn review_report_history(
    bundle_dir: &Path,
    held: &BTreeMap<String, ArtifactManifestFileV2>,
    artifacts: &[TaskArtifact],
    actor: &str,
    now: chrono::DateTime<Utc>,
) -> Result<Option<TaskArtifact>, OrbitError> {
    let Some(report) = artifacts.iter().rev().find(|artifact| {
        normalize_v2_artifact_path(&artifact.path).ok().as_deref() == Some(REVIEW_REPORT_ARTIFACT)
    }) else {
        return Ok(None);
    };
    let Ok(parsed) = ReviewReport::parse(&report.content) else {
        return Ok(None);
    };
    let mut history = match held.get(REVIEW_REPORT_HISTORY_ARTIFACT) {
        Some(file) => {
            let path = resolve_v2_artifact_file_path(bundle_dir, &file.blob)?.ok_or_else(|| {
                OrbitError::Store(format!(
                    "{REVIEW_REPORT_HISTORY_ARTIFACT} blob {} is missing",
                    file.blob
                ))
            })?;
            let content = fs::read(&path).map_err(|err| OrbitError::Io(err.to_string()))?;
            ReviewReportHistory::parse(&content).map_err(OrbitError::Store)?
        }
        None => {
            // A report written before revision retention was introduced is
            // still an obligation source. Import the held report before
            // replacing it, in this same locked manifest rewrite. Otherwise
            // the first new-version put can erase a required failure before
            // settlement ever has a chance to observe it.
            let mut history = ReviewReportHistory::default();
            if let Some(file) = held.get(REVIEW_REPORT_ARTIFACT) {
                let path = resolve_v2_artifact_file_path(bundle_dir, &file.blob)?.ok_or_else(|| {
                    OrbitError::Store(format!(
                        "legacy {REVIEW_REPORT_ARTIFACT} blob {} is missing; refusing to replace evidence whose obligations cannot be established",
                        file.blob
                    ))
                })?;
                let content = fs::read(&path).map_err(|err| OrbitError::Io(err.to_string()))?;
                let legacy = ReviewReport::parse(&content).map_err(|error| {
                    OrbitError::Store(format!(
                        "legacy {REVIEW_REPORT_ARTIFACT} is unreadable; refusing to replace evidence whose obligations cannot be established: {error}"
                    ))
                })?;
                history
                    .record(ReviewReportRevision {
                        attempt_id: legacy.attempt_id,
                        sha256: sha256_hex(&content),
                        observed_at: file.created_at,
                        recorded_by: file.created_by.clone(),
                        verdict: legacy.verdict,
                        validation: legacy.validation,
                    })
                    .map_err(OrbitError::InvalidInput)?;
            }
            history
        }
    };
    let recorded = history
        .record(ReviewReportRevision {
            attempt_id: parsed.attempt_id,
            sha256: sha256_hex(&report.content),
            observed_at: now,
            recorded_by: actor.to_string(),
            verdict: parsed.verdict,
            validation: parsed.validation,
        })
        .map_err(OrbitError::InvalidInput)?;
    if !recorded {
        return Ok(None);
    }
    Ok(Some(TaskArtifact {
        path: REVIEW_REPORT_HISTORY_ARTIFACT.to_string(),
        media_type: "application/json".to_string(),
        content: serde_json::to_vec_pretty(&history)
            .map_err(|err| OrbitError::Store(err.to_string()))?,
        created_by: None,
    }))
}

fn immutable_artifact_blob(path: &str, sha256: &str) -> String {
    // Keep new blobs in the already-durable files directory. The path digest
    // distinguishes equal contents at different logical artifact paths.
    format!(
        "{TASK_ARTIFACT_FILES_DIR_NAME}/.blob-{}-{sha256}",
        sha256_hex(path.as_bytes())
    )
}

impl TaskV2Store {
    pub(crate) fn get_task_artifacts(
        &self,
        id: &str,
    ) -> Result<Option<Vec<TaskArtifact>>, OrbitError> {
        orbit_types::task::validate_orb_task_id(id)?;
        self.ensure_recovered()?;
        let bundle = match self.bundle_store.read_bundle(id) {
            Ok(bundle) => bundle,
            Err(OrbitError::NotFound {
                kind: NotFoundKind::Task,
                ..
            }) => return Ok(None),
            Err(err) => return Err(err),
        };
        let Some(manifest) = bundle.artifact_manifest else {
            return Ok(Some(Vec::new()));
        };
        let bundle_dir = self.bundle_store.bundle_path(id)?;
        let mut artifacts = Vec::new();
        for file in manifest.files {
            let artifact_file = bundle_dir.join(TASK_ARTIFACTS_DIR_NAME).join(&file.blob);
            let content =
                fs::read(&artifact_file).map_err(|err| OrbitError::Io(err.to_string()))?;
            artifacts.push(TaskArtifact {
                path: file.path,
                media_type: file.media_type,
                content,
                created_by: Some(file.created_by),
            });
        }
        artifacts.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(Some(artifacts))
    }

    pub(crate) fn get_task_artifact_manifest(
        &self,
        id: &str,
    ) -> Result<Option<Vec<ArtifactManifestFileV2>>, OrbitError> {
        orbit_types::task::validate_orb_task_id(id)?;
        self.ensure_recovered()?;
        let bundle = match self.bundle_store.read_bundle_lightweight(id) {
            Ok(bundle) => bundle,
            Err(OrbitError::NotFound {
                kind: NotFoundKind::Task,
                ..
            }) => return Ok(None),
            Err(err) => return Err(err),
        };
        let Some(manifest) = bundle.artifact_manifest else {
            return Ok(Some(Vec::new()));
        };
        let mut files = manifest.files;
        files.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(Some(files))
    }

    pub(crate) fn get_task_artifact(
        &self,
        id: &str,
        path: &str,
    ) -> Result<Option<TaskArtifact>, OrbitError> {
        orbit_types::task::validate_orb_task_id(id)?;
        self.ensure_recovered()?;
        let path = normalize_v2_artifact_path(path)?;
        let bundle = match self.bundle_store.read_bundle(id) {
            Ok(bundle) => bundle,
            Err(OrbitError::NotFound {
                kind: NotFoundKind::Task,
                ..
            }) => return Ok(None),
            Err(err) => return Err(err),
        };
        let Some(manifest) = bundle.artifact_manifest else {
            return Ok(None);
        };
        let Some(file) = manifest.files.into_iter().find(|file| file.path == path) else {
            return Ok(None);
        };
        let bundle_dir = self.bundle_store.bundle_path(id)?;
        let Some(artifact_file) = resolve_v2_artifact_file_path(&bundle_dir, &file.blob)? else {
            return Ok(None);
        };
        let content = fs::read(&artifact_file).map_err(|err| OrbitError::Io(err.to_string()))?;
        Ok(Some(TaskArtifact {
            path: file.path,
            media_type: file.media_type,
            content,
            created_by: Some(file.created_by),
        }))
    }

    pub(crate) fn upsert_task_artifacts(
        &self,
        id: &str,
        fields: &TaskArtifactUpdateParams,
    ) -> Result<(), OrbitError> {
        orbit_types::task::validate_orb_task_id(id)?;
        if fields.upsert_artifacts.is_empty() {
            return Ok(());
        }
        if fields.actor.trim().is_empty() {
            return Err(OrbitError::InvalidInput(
                "task actor must not be empty".to_string(),
            ));
        }

        use orbit_types::workflow::automation::{
            COVERAGE_ARTIFACT, CoverageEvidence, EVIDENCE_AUTHORITY_ARTIFACT, EvidenceSubmission,
        };
        let mut artifacts = fields.upsert_artifacts.clone();
        for artifact in &mut artifacts {
            artifact.path = normalize_v2_artifact_path(&artifact.path)?;
            if artifact.path == EVIDENCE_AUTHORITY_ARTIFACT {
                return Err(OrbitError::InvalidInput(
                    "automation evidence authority is reserved for the artifact store".into(),
                ));
            }
            if artifact.path == REVIEW_REPORT_HISTORY_ARTIFACT {
                return Err(OrbitError::InvalidInput(format!(
                    "{REVIEW_REPORT_HISTORY_ARTIFACT} is reserved for the artifact store"
                )));
            }
            // Settlement reads these bytes only after the action stops, when
            // nobody can fix them; refusing here hands the submitter the
            // exact error while its run can still re-put the file.
            if artifact.path == COVERAGE_ARTIFACT
                && let Err(error) = serde_json::from_slice::<CoverageEvidence>(&artifact.content)
            {
                return Err(OrbitError::InvalidInput(format!(
                    "{COVERAGE_ARTIFACT} is not valid coverage evidence: {error}"
                )));
            }
        }
        if let Some(artifact) = artifacts.iter().find(|a| a.path == COVERAGE_ARTIFACT)
            && let Some(run_id) = &fields.owner_run_id
        {
            let witness = EvidenceSubmission {
                action_id: id.into(),
                evidence_digest: sha256_hex(&artifact.content),
                run_id: run_id.clone(),
            };
            artifacts.push(orbit_types::task::TaskArtifact {
                path: EVIDENCE_AUTHORITY_ARTIFACT.into(),
                media_type: "application/json".into(),
                content: serde_json::to_vec(&witness)
                    .map_err(|e| OrbitError::Store(e.to_string()))?,
                created_by: None,
            });
        }
        self.with_task_lock(id, || {
            let mut bundle = self.read_existing_bundle(id)?;
            let bundle_dir = self.bundle_store.bundle_path(id)?;
            let files_dir = bundle_dir
                .join(TASK_ARTIFACTS_DIR_NAME)
                .join(TASK_ARTIFACT_FILES_DIR_NAME);
            fs::create_dir_all(&files_dir).map_err(|err| OrbitError::Io(err.to_string()))?;

            let mut by_path = bundle
                .artifact_manifest
                .take()
                .unwrap_or(ArtifactManifestV2 {
                    schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
                    files: Vec::new(),
                })
                .files
                .into_iter()
                .map(|file| (file.path.clone(), file))
                .collect::<BTreeMap<_, _>>();

            let now = Utc::now();
            let mut artifacts = artifacts.clone();
            if let Some(history) =
                review_report_history(&bundle_dir, &by_path, &artifacts, &fields.actor, now)?
            {
                artifacts.push(history);
            }
            for artifact in &artifacts {
                let path = normalize_v2_artifact_path(&artifact.path)?;
                let sha256 = sha256_hex(&artifact.content);
                let blob = immutable_artifact_blob(&path, &sha256);
                let destination = bundle_dir.join(TASK_ARTIFACTS_DIR_NAME).join(&blob);
                match fs::read(&destination) {
                    Ok(existing) if existing == artifact.content => {}
                    Ok(_) => {
                        return Err(OrbitError::Store(format!(
                            "artifact blob {} has unexpected content",
                            destination.display()
                        )));
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                        atomic_write_bytes(&destination, &artifact.content)
                            .map_err(|err| OrbitError::from_write_io(&destination, err))?;
                    }
                    Err(err) => return Err(OrbitError::Io(err.to_string())),
                }
                by_path.insert(
                    path.clone(),
                    ArtifactManifestFileV2 {
                        origin: fields.origin.clone(),
                        path: path.clone(),
                        blob,
                        sha256,
                        media_type: artifact.media_type.clone(),
                        size_bytes: artifact.content.len() as u64,
                        created_by: fields.actor.clone(),
                        created_at: now,
                    },
                );
            }

            let manifest = ArtifactManifestV2 {
                schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
                files: by_path.into_values().collect(),
            };
            self.bundle_store.rewrite_artifact_manifest(id, &manifest)?;
            let context_files = bundle.envelope.context_files.clone();
            append_creation_grant(
                &self.bundle_store,
                &mut bundle,
                &context_files,
                &[],
                &fields.actor,
                now,
            )?;
            bundle.envelope.updated_at = now;
            self.bundle_store.rewrite_envelope(id, &bundle.envelope)?;
            self.replace_index_best_effort(&bundle.envelope, "task artifact update");
            Ok(())
        })
    }
}
