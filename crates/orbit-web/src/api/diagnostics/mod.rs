//! `/diagnostics/{metrics,errors,friction,implement_one,completions,defects,task_tokens}`
//! aggregation endpoints.

mod audit;
mod completions;
mod defects;
mod durations;
mod errors;
mod friction;
mod metrics;
mod task_tokens;
mod trend;

pub(super) use completions::diagnostics_completions;
pub(super) use defects::diagnostics_defects;
pub(super) use durations::diagnostics_implement_one;
pub(super) use errors::list_diagnostics_errors;
pub(super) use friction::list_diagnostics_friction;
pub(super) use metrics::list_diagnostics_metrics;
pub(super) use task_tokens::diagnostics_task_tokens;
