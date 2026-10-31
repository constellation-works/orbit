//! Condition triggers on the workspace owner
//! (`docs/design/condition-triggers/1_scope.md`): one pass of the firing
//! protocol per definition per clock tick, `when:` guards beside the typed
//! kinds, the enable dry run, `clear-stall`, and the read projections that
//! explain, lists and the dashboard show. Task and workspace scopes only.

mod enable;
mod evaluate;
pub(crate) mod facts;
mod host;
mod runtime;
mod status;

pub use enable::{ConditionEnablePreview, ConditionStallCleared};
pub(crate) use enable::{enable_preview, forget_condition_state, record_condition_disabled};
#[allow(unused_imports)]
pub(crate) use evaluate::evaluate_condition_definition;
pub(crate) use evaluate::{
    ConditionDefinition, condition_consumer, condition_refusal, evaluate_auto_task_condition,
    evaluate_guard, evaluate_routine_condition,
};
pub(crate) use host::{Target, render_placeholders};
pub use status::{ConditionChainLink, ConditionFireView, ConditionListing, ConditionStatus};
pub(crate) use status::{ConditionSubjectView, condition_status};
