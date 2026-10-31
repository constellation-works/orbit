use orbit_types::task::{TaskComment, TaskHistoryEntry, TaskStatus};

/// Desktop-only allowlisted mutation, committed with a payload-bound request receipt.
#[derive(Debug, Clone)]
pub struct DesktopTaskMutationParams {
    pub actor: String,
    pub request_id: String,
    pub payload_digest: String,
    pub expected_revision: String,
    pub fields: orbit_types::desktop::DesktopTaskFields,
    /// Trusted assignment provenance from application validation.
    pub crew_source: Option<Option<String>>,
    pub comment: Option<String>,
    pub status: Option<TaskStatus>,
    /// History committed with the receipt, before it: work the application
    /// must finish after the write even if its first attempt never runs.
    pub append_history: Vec<TaskHistoryEntry>,
}

/// One coherent desktop task read under the ordinary shared bundle read lock.
#[derive(Debug, Clone)]
pub struct DesktopTaskRead {
    pub write_disabled_reason: Option<String>,
    pub task: orbit_types::task::Task,
    pub revision: String,
    pub comments: Vec<TaskComment>,
    pub history: Vec<TaskHistoryEntry>,
    pub artifacts: Vec<orbit_types::task::ArtifactManifestFileV2>,
}
