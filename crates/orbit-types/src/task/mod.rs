//! Domain contracts for this Orbit types module.

mod artifacts;
mod context_creation;
mod context_widening;
mod decision;
mod epic;
mod error;
mod light_delivery;
mod model;
mod os_requirement;
mod plan;
mod readiness;
mod receipt;
mod regression;
mod show_fields;
mod ui_requirement;
mod work_class;
pub use error::TaskError;

pub use context_creation::{
    CONTEXT_CREATION_AUTHORIZED_EVENT, CONTEXT_CREATION_GRANT_VERSION, ContextCreationGrant,
    ContextCreationState, MAX_CONTEXT_CREATION_SELECTORS, context_files_sha256,
};
pub use context_widening::{
    CONTEXT_FILES_WIDENED_EVENT, ContextFilesWidening, ContextWideningStep,
};
pub use decision::{
    FINAL_RECOVERY_DECIDED_EVENT, PILOT_ADMISSION_DECIDED_EVENT, PilotAdmissionVerdict,
    RecordedFinalRecovery, TaskDecision,
};
pub use epic::{EPIC_TAG, EpicHierarchyNode, has_epic_tag, inherited_only_epic_roots};
pub use light_delivery::{
    DEFAULT_LIGHT_DELIVERY_PATHS, LIGHT_DELIVERY_APPLIED_EVENT, LIGHT_DELIVERY_IGNORED_EVENT,
    LIGHT_DELIVERY_PATHS_KEY, LIGHT_DELIVERY_TAGS, LightDeliveryDecision, LightDeliveryJudgement,
    SKIP_REVIEW_TAG, SKIP_VALIDATION_TAG,
};

pub use artifacts::{
    ArtifactManifestFileV2, ArtifactManifestV2, ArtifactWriter, CYCLIC_RELATION_TYPES,
    ORB_TASK_ID_MAX, TASK_ACCEPTANCE_FILE_NAME, TASK_ARTIFACT_FILES_DIR_NAME,
    TASK_ARTIFACT_MANIFEST_FILE_NAME, TASK_ARTIFACT_SCHEMA_VERSION, TASK_ARTIFACTS_DIR_NAME,
    TASK_COMMENTS_FILE_NAME, TASK_DESCRIPTION_FILE_NAME, TASK_ENVELOPE_FILE_NAME,
    TASK_EVENTS_FILE_NAME, TASK_EXECUTION_SUMMARY_FILE_NAME, TASK_PLAN_FILE_NAME,
    TaskArtifactMetadata, TaskCommentRowV2, TaskEnvelopeV2, TaskEventRowV2, TaskRelation,
    TaskRelationEdge, TaskRelationType, canonical_artifact_path, format_task_id,
    is_valid_orb_task_id, is_valid_task_id_prefix, parse_task_number, serialize_task_artifacts,
    task_id_prefix, validate_orb_task_id, validate_relative_artifact_path,
    validate_task_relations_for_source,
};
pub use model::{
    ArtifactPresentation, CANDIDATE_DISCARDED_EVENT, CANDIDATE_RESUME_EVENT,
    CI_FAILURE_KEY_TAG_PREFIX, CLAIM_CANDIDATE_KEPT_EVENT, DEFAULT_TASK_LIST_LIMIT,
    DELIVERY_JOB_TAG_PREFIX, DERIVED_EXECUTION_SUMMARY_PREFIX, DependencyDeadEnd,
    EXECUTION_SUMMARY_DERIVED_EVENT, ExecutionLocation, ExternalRef, GITHUB_PR_EXTERNAL_REF_SYSTEM,
    MAX_TASK_ARTIFACT_CONTENT_BYTES, NO_AUTO_APPROVE_TAG, NO_DIFF_EXPECTED_TAG,
    ResolvedTaskDependency, ResolvedTaskRelation, TASK_REFERENCE_NOT_VERIFIABLE_HERE, Task,
    TaskArtifact, TaskComment, TaskComplexity, TaskCreateStatus, TaskHistoryEntry, TaskOrigin,
    TaskPriority, TaskReferenceIndex, TaskStatus, TaskType, UNSET_BUCKET,
    UnsatisfiableTaskDependency, archived_task_completed_before_archive, artifact_presentation,
    automatic_dispatch_cmp, automatic_dispatch_cmp_for_host, automatic_dispatch_cmp_with_expiry,
    complexity_bucket, complexity_bucket_ord, delivery_job_selection, deserialize_required_tools,
    image_bytes_match_media_type, inline_safe_artifact_media_type, is_inline_image_media_type,
    is_system_identity_tag, is_textual_artifact_media_type, labeled_or_unset,
    media_type_for_artifact_path, names_task_delivery_pull_request, normalize_required_tools,
    normalize_task_dependencies, normalize_task_tags, normalized_artifact_media_type,
    push_external_ref_if_missing, resolve_task_dependencies, resolve_task_dependencies_with_index,
    resolve_task_relations, resolve_task_relations_with_index,
    satisfy_completed_archived_dependencies, task_dependencies_ready_with_index, task_matches_tags,
    unmet_task_dependencies, unmet_task_dependencies_with_index, unsatisfiable_task_dependencies,
    unsatisfiable_task_dependencies_with_index, validate_task_dependencies_with,
};
pub use os_requirement::{
    ExecutorOsLimit, HostOs, OS_TAG_PREFIX, TaskOsRequirement, validate_os_tags,
};
pub use plan::{TaskPlan, TaskPlanCheckpoint, TaskPlanSuccessCriterion};
pub use readiness::{
    ReadinessGap, ReadinessGapCode, ReadinessSeverity, ReadinessStage, TaskReadiness,
    backlog_footprint_warning, declares_no_footprint, readiness_gaps, task_readiness,
    task_readiness_json,
};
pub use receipt::CommentReceipt;
pub use regression::{RegressionClassification, RegressionEscape, RegressionNature};
pub use show_fields::{
    TASK_SHOW_DELIVERY_FIELD, TASK_SHOW_PROJECTION_FIELDS, TASK_SHOW_PROJECTION_FIELDS_CSV,
    TASK_SHOW_PUBLIC_DTO_FIELDS, is_task_show_projection_field, refuses_implementer_writes,
    task_artifact_manifest_json, task_base_json, task_dependency_labels, task_os_requirement_json,
    task_show_record_field_json, task_show_reference_field_json, unknown_task_show_field_message,
};
pub use ui_requirement::{
    BROWSER_KIT_INSTALL_COMMAND, UI_TAG, browser_kit_wait, requires_browser_kit,
};
pub use work_class::{CHANGE_WORK_CLASS, NO_DIFF_WORK_CLASS, auto_task_template, work_class};
