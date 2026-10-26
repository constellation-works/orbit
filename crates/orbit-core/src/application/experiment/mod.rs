//! Experiments: a workspace-local comparison of a control arm against
//! treatment arms, defined in draft and changed only through recorded events
//! once started.

mod definition;
mod lifecycle;
mod view;

pub use lifecycle::{ExperimentActor, ExperimentChange, ExperimentDefinitionInput};
pub use view::{ExperimentArmView, ExperimentEventView, ExperimentView};
