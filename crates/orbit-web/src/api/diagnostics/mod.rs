//! `/diagnostics/{metrics,errors,friction,implement_one,completions,defects,task_tokens,turns_to_edit,diff_stats,brief_accuracy,delivery_friction,backlog_forecast}`
//! aggregation endpoints, and the Analytics › Experiments list, show and
//! readout endpoints.

mod audit;
mod backlog_forecast;
mod brief_accuracy;
mod completions;
mod defects;
mod delivery_friction;
mod diff_stats;
mod durations;
mod errors;
mod experiments;
mod friction;
mod metrics;
mod task_tokens;
mod trend;
mod turns_to_edit;

pub(super) use backlog_forecast::diagnostics_backlog_forecast;
pub(super) use brief_accuracy::diagnostics_brief_accuracy;
pub(super) use completions::diagnostics_completions;
pub(super) use defects::diagnostics_defects;
pub(super) use delivery_friction::diagnostics_delivery_friction;
pub(super) use diff_stats::diagnostics_diff_stats;
pub(super) use durations::diagnostics_implement_one;
pub(super) use errors::list_diagnostics_errors;
pub(super) use experiments::{
    diagnostics_experiment, diagnostics_experiment_readout, diagnostics_experiments,
};
pub(super) use friction::list_diagnostics_friction;
pub(super) use metrics::list_diagnostics_metrics;
pub(super) use task_tokens::diagnostics_task_tokens;
pub(super) use turns_to_edit::diagnostics_turns_to_edit;
