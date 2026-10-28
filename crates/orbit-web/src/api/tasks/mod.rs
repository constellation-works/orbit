//! Task endpoints, request contracts and lifecycle operations.

mod artifacts;
pub(super) mod auto_fix;
pub(super) mod create;
mod lifecycle;
pub(super) mod pilot;
mod reads;
mod records;
mod request;
pub(super) mod triage;
mod writes;

pub(super) use artifacts::get_task_artifact;
pub(super) use lifecycle::{
    approve_task_action, archive_task_action, reject_task_action, requeue_task_action,
    settle_merged_task_action, settle_merged_tasks_action,
};
pub(super) use reads::{completion_by_complexity, get_task, list_task_locks, list_tasks};
pub(super) use records::{
    pilot_admission_task_action, post_deploy_met_task_action, post_deploy_task_action,
};
pub(super) use writes::{
    add_task_comment_action, create_task_action, human_comment_author, update_task_action,
};

/// Actor recorded in task history for dashboard recovery actions.
const DASHBOARD_ACTOR: &str = "dashboard";
