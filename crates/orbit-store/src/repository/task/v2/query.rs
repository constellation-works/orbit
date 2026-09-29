use orbit_common::text::contains_lowercased;

use super::*;

impl TaskV2Store {
    /// Search over the bundles the candidate listing already read, so a query
    /// costs one lightweight bundle read per task. Interactive task search
    /// never opens artifact payloads; only the manifest paths participate.
    pub(super) fn search_bundles(
        &self,
        bundles: Vec<TaskBundleV2>,
        lowered: &str,
    ) -> Result<Vec<Task>, OrbitError> {
        let mut matches = Vec::new();
        for bundle in bundles {
            let sidecars_match = bundle
                .comments
                .iter()
                .any(|comment| contains_lowercased(&comment.body, lowered))
                || artifact_manifest_path_matches_query(bundle.artifact_manifest.as_ref(), lowered);
            let task = self.task_from_bundle(bundle)?;
            if task_in_memory_fields_match_query(&task, lowered) || sidecars_match {
                matches.push(task);
            }
        }
        Ok(matches)
    }
}

fn artifact_manifest_path_matches_query(
    manifest: Option<&ArtifactManifestV2>,
    lowered: &str,
) -> bool {
    manifest.is_some_and(|manifest| {
        manifest
            .files
            .iter()
            .any(|file| contains_lowercased(&file.path, lowered))
    })
}

fn task_in_memory_fields_match_query(task: &Task, lowered: &str) -> bool {
    contains_lowercased(&task.title, lowered)
        || contains_lowercased(&task.description, lowered)
        || contains_lowercased(&task.plan, lowered)
        || contains_lowercased(&task.execution_summary, lowered)
        || task
            .acceptance_criteria
            .iter()
            .any(|criterion| contains_lowercased(criterion, lowered))
        || task.external_refs.iter().any(|external_ref| {
            contains_lowercased(&external_ref.system, lowered)
                || contains_lowercased(&external_ref.id, lowered)
        })
}
