mod completion;
mod cursor;
mod definition;

pub use completion::{
    AUTO_TASK_ACCEPTED_HISTORY_SCHEMA_VERSION, AUTO_TASK_EVIDENCE_SCHEMA_VERSION,
    AutoTaskAcceptedHistory, AutoTaskAcceptedHistoryFile, AutoTaskAdapterVerdict,
    AutoTaskCompletionContract, AutoTaskCompletionReceipt, AutoTaskEvidenceBinding,
    AutoTaskEvidenceCandidate, AutoTaskEvidenceRef, AutoTaskResultStatus, AutoTaskVerdictReason,
};
pub use cursor::{
    AutoTaskCursor, AutoTaskCursorState, AutoTaskPendingClaim, AutoTaskSkipRecord,
    InactivePluginWarning,
};
pub use definition::{
    AUTO_TASK_SCHEMA_VERSION, AUTO_TASK_TAG_PREFIX, AutoTaskDefinition, AutoTaskSchedule,
    AutoTaskTemplate, DedupePolicy, MAX_AUTO_TASK_INTERVAL_MINUTES, SWEEP_CURSOR_ARTIFACT,
    SWEEP_CURSOR_SCHEMA_VERSION, SkipIfUnchanged, SweepCursorRecord, SweepCursorSelector,
    auto_task_builds_by_design, auto_task_tag, files_regression_findings, is_valid_auto_task_name,
};
