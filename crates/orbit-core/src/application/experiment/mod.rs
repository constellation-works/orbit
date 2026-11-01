//! Experiments: a workspace-local comparison of a control arm against
//! treatment arms, defined in draft and changed only through recorded events
//! once started.

mod assignment;
mod capture;
mod definition;
pub(crate) use capture::validate_experiment_capture;
pub use definition::{
    EXPERIMENT_COMPLEXITIES, EXPERIMENT_METRICS, STEP_LOCAL_METRICS,
    experiment_metric_is_step_local,
};
mod lifecycle;
mod overlay;
mod progress;
mod promotion;
mod readout;
mod run_policy;
mod skills;
mod templates;
mod view;

pub use lifecycle::{ExperimentActor, ExperimentChange, ExperimentDefinitionInput};
pub use readout::ExperimentReadout;
pub(crate) use templates::experiment_definition_from_template;
pub use view::{ExperimentArmView, ExperimentEventView, ExperimentView};
