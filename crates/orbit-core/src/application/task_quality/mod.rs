//! Shadow task-quality consumer. The committed task is the reconciliation
//! source; a failed local scheduling write cannot lose work permanently.
mod contract;
mod ledger;
mod runtime;

pub use contract::{TASK_QUALITY_USE_CASE, task_quality_question_template};
pub use ledger::{
    TaskQualityAnnotation, TaskQualityDismissal, TaskQualityDismissalReason, TaskQualityHistory,
    TaskQualitySnapshot,
};
