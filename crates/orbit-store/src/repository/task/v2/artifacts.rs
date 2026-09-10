use super::*;

impl TaskV2Store {
    pub(crate) fn get_task_artifacts(
        &self,
        id: &str,
    ) -> Result<Option<Vec<TaskArtifact>>, OrbitError> {
        orbit_types::task::validate_orb_task_id(id)?;
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
        let Some(artifact_file) = resolve_v2_artifact_file_path(&bundle_dir, &file.path)? else {
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

        use orbit_types::workflow::automation::{EVIDENCE_AUTHORITY_ARTIFACT, EvidenceSubmission};
        let mut artifacts = fields.upsert_artifacts.clone();
        for artifact in &mut artifacts {
            artifact.path = normalize_v2_artifact_path(&artifact.path)?;
            if artifact.path == EVIDENCE_AUTHORITY_ARTIFACT {
                return Err(OrbitError::InvalidInput(
                    "automation evidence authority is reserved for the artifact store".into(),
                ));
            }
        }
        if let Some(artifact) = artifacts
            .iter()
            .find(|a| a.path == "automation-coverage.json")
            && let Some(run_id) = &fields.owner_run_id
        {
            let witness = EvidenceSubmission {
                action_id: id.into(),
                evidence_digest: format!("{:x}", Sha256::digest(&artifact.content)),
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
            for artifact in &artifacts {
                let path = normalize_v2_artifact_path(&artifact.path)?;
                let blob = format!("{TASK_ARTIFACT_FILES_DIR_NAME}/{path}");
                let destination = files_dir.join(&path);
                atomic_write_bytes(&destination, &artifact.content)
                    .map_err(|err| OrbitError::Io(err.to_string()))?;
                by_path.insert(
                    path.clone(),
                    ArtifactManifestFileV2 {
                        path: path.clone(),
                        blob,
                        sha256: format!("{:x}", Sha256::digest(&artifact.content)),
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
            bundle.envelope.updated_at = now;
            self.bundle_store.rewrite_envelope(id, &bundle.envelope)?;
            self.replace_index_best_effort(&bundle.envelope, "task artifact update");
            Ok(())
        })
    }
}
