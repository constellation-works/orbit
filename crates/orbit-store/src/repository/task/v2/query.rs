use super::*;

impl TaskV2Store {
    /// Search over the bundles the candidate listing already read, so a
    /// query costs one lightweight bundle read per task. Materialization does
    /// not hash artifact payloads; a later on-demand read may open a text
    /// blob only when in-memory fields and comments did not already match.
    pub(super) fn search_bundles(
        &self,
        bundles: Vec<TaskBundleV2>,
        lowered: &str,
    ) -> Result<Vec<Task>, OrbitError> {
        let mut matches = Vec::new();
        for bundle in bundles {
            let comments_match = bundle
                .comments
                .iter()
                .any(|comment| comment.body.to_lowercase().contains(lowered));
            let manifest = bundle.artifact_manifest.clone();
            let task = self.task_from_bundle(bundle)?;
            // Cheapest evidence first: the fields already in memory, then the
            // comments, and only then the artifact blobs on disk.
            if task_in_memory_fields_match_query(&task, lowered)
                || comments_match
                || self.artifact_manifest_matches_query(&task.id, manifest.as_ref(), lowered)?
            {
                matches.push(task);
            }
        }
        Ok(matches)
    }

    /// Phase 5 bridge: artifact search reads text artifact files on demand
    /// until generated full-text indexes carry artifact paths, content, and
    /// snippets. A path match needs no read; a text blob is read only when
    /// its path did not already match.
    fn artifact_manifest_matches_query(
        &self,
        id: &str,
        manifest: Option<&ArtifactManifestV2>,
        lowered: &str,
    ) -> Result<bool, OrbitError> {
        let Some(manifest) = manifest else {
            return Ok(false);
        };
        let artifact_dir = self
            .bundle_store
            .bundle_path(id)?
            .join(TASK_ARTIFACTS_DIR_NAME);
        for file in &manifest.files {
            if file.path.to_lowercase().contains(lowered) {
                return Ok(true);
            }
            if !is_text_artifact_media_type(&file.media_type) {
                continue;
            }
            let content = match fs::read(artifact_dir.join(&file.blob)) {
                Ok(content) => content,
                // The bundle can be rewritten between the listing and this
                // read; a blob that vanished is not a match, not an error.
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => return Err(OrbitError::Io(err.to_string())),
            };
            if String::from_utf8(content)
                .ok()
                .is_some_and(|text| text.to_lowercase().contains(lowered))
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

fn task_in_memory_fields_match_query(task: &Task, lowered: &str) -> bool {
    task.title.to_lowercase().contains(lowered)
        || task.description.to_lowercase().contains(lowered)
        || task.plan.to_lowercase().contains(lowered)
        || task.execution_summary.to_lowercase().contains(lowered)
        || task
            .acceptance_criteria
            .iter()
            .any(|criterion| criterion.to_lowercase().contains(lowered))
        || task.external_refs.iter().any(|external_ref| {
            external_ref.system.to_lowercase().contains(lowered)
                || external_ref.id.to_lowercase().contains(lowered)
        })
}

fn is_text_artifact_media_type(media_type: &str) -> bool {
    let base = media_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    base.starts_with("text/")
        || matches!(
            base.as_str(),
            "application/json"
                | "application/javascript"
                | "application/toml"
                | "application/x-toml"
                | "application/x-yaml"
                | "application/xml"
                | "application/yaml"
        )
        || base.ends_with("+json")
        || base.ends_with("+xml")
}
