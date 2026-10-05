use orbit_common::text::contains_lowercased;
use orbit_types::task::ExternalRef;

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
        let sidecars_match =
            sidecar_fields_match(&bundle.comments, bundle.artifact_manifest.as_ref(), lowered);
        let task = self.task_from_bundle(bundle)?;
        Ok((task_in_memory_fields_match_query(&task, lowered) || sidecars_match).then_some(task))
    }

    /// Whether the task `envelope` describes could match `lowered`, decided
    /// from the envelope and the search documents alone: no event log, no
    /// consistency checks, no [`Task`] built. Every field is matched by the
    /// same helpers [`Self::matching_task`] uses.
    ///
    /// A pruning check, never a verdict: `true` also covers "cannot tell" (a
    /// pending write, a bundle in flight, a read failure), so the caller's full
    /// read decides and reports any failure. `false` is only ever the answer
    /// for a task whose files were read and hold no match.
    pub(super) fn may_match(&self, envelope: &TaskEnvelopeV2, lowered: &str) -> bool {
        if envelope_fields_match(&envelope.title, &envelope.external_refs, lowered) {
            return true;
        }
        let Some(docs) = self.bundle_store.read_search_docs(&envelope.id) else {
            return true;
        };
        body_fields_match(
            &docs.description,
            &docs.plan,
            &docs.execution_summary,
            &parse_acceptance(&docs.acceptance),
            lowered,
        ) || sidecar_fields_match(&docs.comments, docs.artifact_manifest.as_ref(), lowered)
    }
}

fn sidecar_fields_match(
    comments: &[TaskCommentRowV2],
    manifest: Option<&ArtifactManifestV2>,
    lowered: &str,
) -> bool {
    comments
        .iter()
        .any(|comment| contains_lowercased(&comment.body, lowered))
        || artifact_manifest_path_matches_query(manifest, lowered)
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
    envelope_fields_match(&task.title, &task.external_refs, lowered)
        || body_fields_match(
            &task.description,
            &task.plan,
            &task.execution_summary,
            &task.acceptance_criteria,
            lowered,
        )
}

fn envelope_fields_match(title: &str, external_refs: &[ExternalRef], lowered: &str) -> bool {
    contains_lowercased(title, lowered)
        || external_refs.iter().any(|external_ref| {
            contains_lowercased(&external_ref.system, lowered)
                || contains_lowercased(&external_ref.id, lowered)
        })
}

fn body_fields_match(
    description: &str,
    plan: &str,
    execution_summary: &str,
    acceptance_criteria: &[String],
    lowered: &str,
) -> bool {
    contains_lowercased(description, lowered)
        || contains_lowercased(plan, lowered)
        || contains_lowercased(execution_summary, lowered)
        || acceptance_criteria
            .iter()
            .any(|criterion| contains_lowercased(criterion, lowered))
}
