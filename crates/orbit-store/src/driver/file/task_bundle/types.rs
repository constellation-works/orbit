use orbit_types::task::{
    ArtifactManifestV2, TASK_ACCEPTANCE_FILE_NAME, TASK_DESCRIPTION_FILE_NAME,
    TASK_EXECUTION_SUMMARY_FILE_NAME, TASK_PLAN_FILE_NAME, TaskCommentRowV2, TaskEnvelopeV2,
    TaskEventRowV2,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TaskBundleV2 {
    pub(crate) envelope: TaskEnvelopeV2,
    pub(crate) description: String,
    pub(crate) acceptance: String,
    pub(crate) plan: String,
    pub(crate) execution_summary: String,
    pub(crate) events: Vec<TaskEventRowV2>,
    pub(crate) comments: Vec<TaskCommentRowV2>,
    pub(crate) artifact_manifest: Option<ArtifactManifestV2>,
}

/// The documents task search matches on besides the envelope, read without
/// the event log. See [`super::bundle_io::read_search_docs_at`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TaskSearchDocs {
    pub(crate) description: String,
    pub(crate) acceptance: String,
    pub(crate) plan: String,
    pub(crate) execution_summary: String,
    pub(crate) comments: Vec<TaskCommentRowV2>,
    pub(crate) artifact_manifest: Option<ArtifactManifestV2>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskDocumentV2 {
    Description,
    Acceptance,
    Plan,
    ExecutionSummary,
}

impl TaskDocumentV2 {
    pub(crate) fn file_name(self) -> &'static str {
        match self {
            Self::Description => TASK_DESCRIPTION_FILE_NAME,
            Self::Acceptance => TASK_ACCEPTANCE_FILE_NAME,
            Self::Plan => TASK_PLAN_FILE_NAME,
            Self::ExecutionSummary => TASK_EXECUTION_SUMMARY_FILE_NAME,
        }
    }
}
