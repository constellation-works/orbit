//! Domain contracts for this Orbit types module.

mod artifacts;
mod error;
mod model;
mod plan;
mod show_fields;
pub use error::TaskError;

#[cfg(test)]
mod tests;

pub use artifacts::{
    ArtifactManifestFileV2, ArtifactManifestV2, ORB_TASK_ID_MAX, ORB_TASK_ID_PREFIX,
    ORB_TASK_ID_WIDTH, TASK_ACCEPTANCE_FILE_NAME, TASK_ARTIFACT_FILES_DIR_NAME,
    TASK_ARTIFACT_MANIFEST_FILE_NAME, TASK_ARTIFACT_SCHEMA_VERSION, TASK_ARTIFACTS_DIR_NAME,
    TASK_COMMENTS_FILE_NAME, TASK_DESCRIPTION_FILE_NAME, TASK_ENVELOPE_FILE_NAME,
    TASK_EVENTS_FILE_NAME, TASK_EXECUTION_SUMMARY_FILE_NAME, TASK_PLAN_FILE_NAME,
    TaskArtifactMetadata, TaskCommentRowV2, TaskEnvelopeV2, TaskEventRowV2, TaskRelation,
    TaskRelationEdge, TaskRelationType, format_orb_task_id, format_task_id, is_valid_orb_task_id,
    is_valid_task_id_prefix, parse_task_number, serialize_task_artifacts, task_id_prefix,
    validate_orb_task_id, validate_relative_artifact_path, validate_task_relations_for_source,
};
pub use model::{
    ArtifactPresentation, DEFAULT_TASK_LIST_LIMIT, DependencyDeadEnd, ExternalRef,
    GITHUB_PR_EXTERNAL_REF_SYSTEM, MAX_TASK_ARTIFACT_CONTENT_BYTES, NO_DIFF_EXPECTED_TAG,
    ResolvedTaskDependency, ResolvedTaskRelation, TASK_REFERENCE_NOT_VERIFIABLE_HERE, Task,
    TaskArtifact, TaskComment, TaskComplexity, TaskCreateStatus, TaskHistoryEntry, TaskPriority,
    TaskReferenceIndex, TaskStatus, TaskType, UNSET_BUCKET, UnsatisfiableTaskDependency,
    artifact_presentation, build_task_status_index, complexity_bucket, complexity_bucket_ord,
    deserialize_required_tools, image_bytes_match_media_type, inline_safe_artifact_media_type,
    is_inline_image_media_type, is_textual_artifact_media_type, labeled_or_unset,
    media_type_for_artifact_path, normalize_required_tools, normalize_task_dependencies,
    normalize_task_tags, normalized_artifact_media_type, push_external_ref_if_missing,
    resolve_task_dependencies, resolve_task_dependencies_with_index, resolve_task_relations,
    resolve_task_relations_with_index, task_dependencies_ready, task_dependencies_ready_with_index,
    task_matches_tags, task_reference_is_not_verifiable_here, unmet_task_dependencies,
    unmet_task_dependencies_with_index, unsatisfiable_task_dependencies,
    unsatisfiable_task_dependencies_with_index, validate_task_dependencies,
};
pub use plan::{TaskPlan, TaskPlanCheckpoint, TaskPlanSuccessCriterion};
pub use show_fields::{
    TASK_SHOW_DERIVED_RESPONSE_FIELDS, TASK_SHOW_PROJECTION_FIELDS,
    TASK_SHOW_PROJECTION_FIELDS_CSV, TASK_SHOW_PUBLIC_DTO_FIELDS, is_task_show_projection_field,
    task_show_record_field_json, unknown_task_show_field_message,
};
