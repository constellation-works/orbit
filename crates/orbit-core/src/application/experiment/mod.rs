//! Experiments: a workspace-local comparison of a control arm against
//! treatment arms, defined in draft and changed only through recorded events
//! once started.

mod assignment;
mod capture;
mod definition;
mod distributed_port;
mod job_port;
mod lifecycle;
mod memory_arms;
mod overlay;
mod progress;
mod promotion;
mod readout;
mod records;
mod run_policy;
mod skills;
mod tag_instruction_port;
mod task_port;
mod view;

pub(crate) use capture::validate_claimed_experiment_capture;
use chrono::{DateTime, Datelike, Days, Utc};
pub use definition::{
    EXPERIMENT_COMPLEXITIES, experiment_metric_is_review_derived, experiment_metric_is_step_local,
    experiment_metric_names, step_local_metric_names,
};
pub use lifecycle::{ExperimentActor, ExperimentChange, ExperimentDefinitionInput};
pub(crate) use memory_arms::memory_arm_names;
pub use progress::ExperimentProgress;
pub use readout::ExperimentReadout;
pub use records::{ExperimentAssignmentDecision, assign_experiments, write_experiment};
pub use view::{ExperimentArmView, ExperimentEventView, ExperimentView};

/// Experiments that may run at once in one workspace.
///
/// The former `experiments.max_running` default, fixed when the key was removed [ORB-22808].
pub const EXPERIMENTS_MAX_RUNNING: u32 = 3;

/// Share of a treatment arm's settled tasks that must have received its
/// treatment; below it, a readout gives no verdict.
///
/// The former `experiments.min_delivered_rate` default, fixed when the key was removed [ORB-22808].
pub(crate) const EXPERIMENTS_MIN_DELIVERED_RATE: f64 = 0.9;

/// Use the four-digit RFC 3339 calendar shared by the API and the dashboard.
fn experiment_horizon_at(start: DateTime<Utc>, days: u32) -> Option<DateTime<Utc>> {
    start
        .checked_add_days(Days::new(u64::from(days)))
        .filter(|end| days > 0 && (1..=9999).contains(&end.year()))
}
