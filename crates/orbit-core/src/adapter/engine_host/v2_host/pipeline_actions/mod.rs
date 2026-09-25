//! Deterministic actions pipeline sequencers use to validate bundles, dispatch
//! child Jobs, re-check admission at dispatch, and guard pipeline results.

use orbit_engine::DispatchError;

mod bundles;
mod gate_admission;
mod gate_starvation;
mod invoke;
mod results;

pub(super) use bundles::validate_bundles;
pub(super) use gate_starvation::gate_starvation_fail;
pub(super) use invoke::{invoke_and_wait, invoke_detached};
pub(super) use results::{pipeline_success_guard, record_pipeline_results_audit};

#[cfg(test)]
mod tests;

fn action_failed(action: &str, message: String) -> DispatchError {
    DispatchError::DeterministicActionFailed {
        action: action.to_string(),
        message,
    }
}
