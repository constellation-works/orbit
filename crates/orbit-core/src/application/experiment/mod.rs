//! Experiments: a workspace-local comparison of a control arm against
//! treatment arms, defined in draft and changed only through recorded events
//! once started.

mod assignment;
mod capture;
mod definition;
mod distributed_port;
mod job_port;
pub(crate) use capture::validate_claimed_experiment_capture;
pub use definition::{
    EXPERIMENT_COMPLEXITIES, EXPERIMENT_METRICS, STEP_LOCAL_METRICS,
    experiment_metric_is_step_local,
};
mod lifecycle;
mod memory_arms;
pub(crate) use memory_arms::memory_arm_names;
mod overlay;
mod progress;
mod promotion;
mod readout;
mod run_policy;
mod skills;
mod tag_instruction_port;
mod task_port;
mod view;

pub use lifecycle::{ExperimentActor, ExperimentChange, ExperimentDefinitionInput};

/// Experiments that may run at once in one workspace.
///
/// The former `experiments.max_running` default, fixed when the key was removed [ORB-22808].
pub const EXPERIMENTS_MAX_RUNNING: u32 = 3;

/// Share of a treatment arm's settled tasks that must have received its
/// treatment; below it, a readout gives no verdict.
///
/// The former `experiments.min_delivered_rate` default, fixed when the key was removed [ORB-22808].
pub const EXPERIMENTS_MIN_DELIVERED_RATE: f64 = 0.9;
pub use readout::ExperimentReadout;
pub use view::{ExperimentArmView, ExperimentEventView, ExperimentView};
