mod cursor;
mod definition;

pub use cursor::{AutoTaskCursor, AutoTaskCursorState, AutoTaskPendingClaim, AutoTaskSkipRecord};
pub use definition::{
    AUTO_TASK_SCHEMA_VERSION, AUTO_TASK_TAG_PREFIX, AutoTaskDefinition, AutoTaskSchedule,
    AutoTaskTemplate, DedupePolicy, MAX_AUTO_TASK_INTERVAL_MINUTES, SWEEP_CURSOR_ARTIFACT,
    SWEEP_CURSOR_SCHEMA_VERSION, SkipIfUnchanged, SweepCursorRecord, SweepCursorSelector,
    auto_task_tag, is_valid_auto_task_name,
};
