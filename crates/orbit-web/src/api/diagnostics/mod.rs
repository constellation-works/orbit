//! `/diagnostics/{metrics,errors,friction}` aggregation endpoints.

mod audit;
mod durations;
mod errors;
mod friction;
mod metrics;

pub(super) use durations::diagnostics_implement_one;
pub(super) use errors::list_diagnostics_errors;
pub(super) use friction::list_diagnostics_friction;
pub(super) use metrics::list_diagnostics_metrics;
