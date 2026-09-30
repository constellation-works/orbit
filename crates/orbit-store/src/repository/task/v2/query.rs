use orbit_common::text::contains_lowercased;

use super::*;

impl TaskV2Store {
    /// The task `bundle` describes when it matches `lowered`, else `None`.
    /// Matching is over the lightweight bundle read: it never opens artifact
    /// payloads, and only the manifest paths participate.
    pub(super) fn matching_task(
        &self,
        bundle: TaskBundleV2,
        lowered: &str,
    ) -> Result<Option<Task>, OrbitError> {
        let sidecars_match = bundle
            .comments
            .iter()
            .any(|comment| contains_lowercased(&comment.body, lowered))
            || artifact_manifest_path_matches_query(bundle.artifact_manifest.as_ref(), lowered);
        let task = self.task_from_bundle(bundle)?;
        Ok((task_in_memory_fields_match_query(&task, lowered) || sidecars_match).then_some(task))
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
