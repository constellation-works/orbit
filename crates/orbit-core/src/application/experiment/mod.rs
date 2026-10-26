//! Experiments: a workspace-local comparison of a control arm against
//! treatment arms, defined in draft and changed only through recorded events
//! once started.

mod assignment;
mod capture;
mod definition;
pub(crate) use capture::validate_experiment_capture;
mod lifecycle;
mod overlay;
mod readout;
mod run_policy;
mod view;

pub use lifecycle::{ExperimentActor, ExperimentChange, ExperimentDefinitionInput};
pub use readout::ExperimentReadout;
pub use view::{ExperimentArmView, ExperimentEventView, ExperimentView};
